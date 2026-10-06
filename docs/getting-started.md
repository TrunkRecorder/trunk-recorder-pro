# Getting Started

This page walks you through a first run, from looking up your radio system to hearing the first
recorded call. It assumes Trunk Recorder Pro is already installed; if it isn't, start with
[Installing](install/README.md).

It takes a little research to set up a trunked system, but once it's working you shouldn't have
to touch it again. Most of the work is done for you: **Find my system** scans the air, finds the
control channel, measures your radio's frequency error and gain, and fills in the settings.

## How trunking works, briefly

A trunked radio system shares a handful of frequencies among many groups of users
(talkgroups). One frequency, the **control channel**, transmits all the time. It tells the radios
on the system which talkgroup is about to talk on which voice frequency. Trunk Recorder Pro
listens to the control channel, and each time it announces a call (a *grant*), it tunes a
recorder to that voice frequency and records the call.

That means two things have to be right:

- Your radio has to hear the control channel clearly. If it doesn't, nothing is recorded.
- The voice frequencies have to fall inside the slice of spectrum your radio (or radios) covers.
  A call on a frequency no radio covers is skipped.

## Research the system

Before you start, look the system up on [RadioReference](https://www.radioreference.com/db/).
Search for your county, then pick the system you want to record. Take note of:

- **System Type.** Trunk Recorder Pro records P25 (Phase 1 and Phase 2), Motorola SmartNet /
  SmartZone, trunked DMR (Capacity Plus, Capacity Max, Connect Plus, Tier III) and NXDN. It also
  records conventional (non-trunked) channels; see [Conventional channels](guides/conventional.md).
- **The control channels.** On a site's frequency list, control channels are marked in red, or
  with a "c". A site usually has one active control channel and one or more alternates.
- **The range of frequencies.** Look at the lowest and highest frequency of the site you want. An
  RTL-SDR at 2.4 MSPS covers a bit over 2 MHz; if the site spreads further, you need a second
  radio or a wider one to catch every call. [Radios](guides/radios.md) explains coverage.
- **System ID, WACN and NAC** (P25), if listed. You don't need to type them in, but they help you
  confirm you found the right system.

You don't strictly need any of this: Find my system can find a system with nothing configured.
The research helps when the scan finds several systems, or when it finds nothing.

## Check your radios

Plug your SDR straight into the computer (hubs and extension cables cause trouble) and quit any
other program that might be using it, such as SDR#, SDR++, GQRX or rtl_tcp. Only one program can
use a radio at a time.

RTL-SDR dongles work with nothing else installed (on Windows, after the one-time WinUSB driver
install with Zadig; on Linux, after the udev rule). USRP, Airspy and SoapySDR radios need their
makers' drivers: see [Other radios](install/other-radios.md).

To see what Trunk Recorder Pro can find from a terminal:

```bash
trunk-pro devices
```

It lists the RTL-SDRs (with their serial numbers), Airspys and SoapySDR devices, and whether the
optional USRP, Airspy and SoapySDR drivers are installed. Add `--usrp` to search for USRPs too.

## First launch: the setup guide

Start the app (open **Trunk Recorder Pro**, or run `trunk-pro` in a terminal). It opens its
interface in your browser at `http://localhost:8080`.

The first time, when nothing is set up yet, the **Setup guide** opens. You can also open it later
with the **Setup guide** button at the top of the page whenever the recorder is stopped. Every
step edits the same settings as the Setup page, so leaving part way keeps what you've done.

The welcome screen offers three ways in:

- **Get started** runs the guide.
- **I have a Trunk Recorder config** imports an existing Trunk Recorder `config.json` instead.
  See [Migrating from Trunk Recorder](migrating-from-trunk-recorder.md).
- **Skip to the app** goes straight to the interface, to set everything up by hand.

The guide has six steps:

1. **Radios.** It lists every radio it can see (RTL-SDRs, Airspys, USRPs and SoapySDR devices)
   and asks *Which radios should record?* All free ones are ticked. A radio another program is
   using shows as busy. If none shows up, **Help me find it** walks through the usual fixes for
   your operating system (USB driver, udev rule, TV-tuner driver, other SDR apps).
2. **Find a system.** *Find a system near you* scans the bands for control channels (pick which
   bands under **Bands to scan**; the defaults cover most public-safety systems) and takes a
   minute or two. Press **Start scan**. When it finds a P25 or SmartNet control channel it
   listens to it, ticking off *Hearing it clearly*, *Who it is*, *Radio tuning* (the frequency
   correction) and *Voice channels*. When enough is known, press **Use this system**. Found
   trunked DMR or NXDN sites are shown as cards to pick instead. If you already know the control
   channel, **I know the control channel** lets you type it in and pick the system type (P25,
   SmartNet, DMR or NXDN).
3. **Coverage.** *Where each radio listens* shows the control channel and the voice channels the
   scan saw, with each radio's slice of spectrum over them. It places the radios over the
   channels that carried the most calls and tells you how many voice channels are in reach and
   what share of calls that is. You can drag each radio's center with its slider. You can't go on
   until the control channel is in reach.
4. **Name.** *Name your system*: a **System name** for display, and a **Short name**, which is the
   name of its folder in the recordings and how plugins refer to it.
5. **Storage.** *Where to save calls*: the default folder, or **Somewhere else** (a bigger drive,
   a shared folder). It also shows where your settings file is kept. (The browser version has no
   such step: calls stay in the browser.)
6. **Talkgroups.** *Name the talkgroups*, optional: **Copy from RadioReference** (select the
   talkgroup table on the system's RadioReference page, copy it, and paste it in), **I have a
   file** (a talkgroup CSV), or **Not now**. Calls are recorded either way. See
   [Talkgroups and units](guides/talkgroups-and-units.md).

The last screen, *You're all set*, sums up what was set up. Press **Start recording**, or **Open
the app without starting**.

## Setting it up without the guide

Everything the guide does can be done on the **Setup** page, which has five tabs: **Systems**,
**Conventional**, **Radios**, **Recording** and **Plugins**.

### Find my system

On the **Systems** tab, the **Find my system** panel runs the same scan as the guide. Pick the
bands, press **Scan**, and when it's listening to a control channel and shows **Ready to record**,
press **Add this system**. That adds the system with its control channels and a site lock, and
sets the radio's frequency correction, gain and center frequency. The details, including the
command-line version, are in [Find my system](guides/find-my-system.md).

### Entering a system by hand

If you know the control channels, press **Add a system** on the **Systems** tab and fill in:

- **Short name**: the folder its calls go in (letters, digits, `.`, `_` and `-`).
- **Name**: optional, for display.
- **Type**: **P25**, **SmartNet / SmartZone**, **DMR (trunked)** or **NXDN (trunked)**.
- **Control channels, MHz**: the site's control channels, separated by commas
  (`857.9875, 858.9875`). It starts on the first; later ones are fallbacks, tried in turn when the
  control channel goes quiet.
- **Modulation** (P25): leave it on **Auto (both receivers)** unless you know better.
- For SmartNet, the **Band plan**: see [SmartNet](guides/smartnet.md). For DMR and NXDN, see
  [DMR](guides/dmr.md) and [NXDN](guides/nxdn.md).

Each system card shows whether its control channel lands inside a source ("control channel on
source 1") or not ("no control channel inside a source"). The other keys are described in the
[configuration reference](configuration/systems.md).

## Set up the radio

The **Radios** tab has a card per source (radio). The settings that matter for a first run:

### Gain

Gain is how much the radio amplifies what the antenna picks up. With too little, the control
channel is too weak to decode. With too much, strong signals overload the radio and distort
everything else, and the noise comes up with the signal.

If you used Find my system with an RTL-SDR, the gain is already set: it tries a range of gains
while listening to the control channel and keeps the lowest one that decodes as well as the best.
Otherwise, start from the default (25.4 dB on an RTL-SDR) and leave **AGC** off; as the hint says,
a fixed gain usually works better. Once recording, the dashboard warns you if the gain is too
high (samples at full scale). [Radios](guides/radios.md#gain-and-agc) has more on gain for each
type of radio.

### Frequency correction (ppm)

Cheap SDRs don't tune exactly where they're told. The error is measured in parts per million
(ppm): an error of 2 ppm at 858 MHz puts every signal about 1.7 kHz off. Digital control channels
are narrow, so a few ppm can be the difference between decoding and not.

Find my system measures this for you: the control channel announces its own frequency, and base
stations are locked to GPS, so how far off your radio hears it is your radio's error. It sets
**Frequency correction, ppm** (whole numbers on an RTL-SDR). You can also turn on **AutoTune**,
which keeps measuring the error on the control channel while recording and corrects for drift.
See [ppm and AutoTune](guides/radios.md#frequency-correction-ppm-and-autotune).

### Center frequency

The **Center frequency, MHz** is the middle of the slice of spectrum the radio covers. Leave it
blank (**Auto**) and the recorder places the radio over the system's control channels and the
voice channels it knows about; the hint under the field shows where (`Auto: 858.3000 MHz`).
Find my system sets a center that covers as many of the voice channels it saw as one radio can.
You only need to type a center if you want to place the radio yourself.

The bar under each source card shows its usable range, with a tall tick for each control channel
and a short tick for each known voice channel inside it, in each system's colour. If the control
channel isn't on any radio's bar, nothing will record. See
[Center frequency and Auto](guides/radios.md#center-frequency-and-auto).

## Start recording

Press **Start** at the top right. The status pill changes from *Stopped* to *Starting…* and then
*Recording*. If something is missing (no source, no system, a radio that couldn't be placed),
**Start** is greyed out and its tooltip says why.

To have the recorder start by itself when the app starts, turn on **Start recording when the app
starts** on the **Recording** tab, or run `trunk-pro --start`.

## Check that it works

Open the **Overview** page. Each system has a card; while recording, the main figure is the
**Control channel** rate in messages per second, with the share of messages decoded, the control
channel frequency (or *hunting* while it looks for one) and its signal-to-noise ratio.

- A steady rate with most messages decoded means you're locked on. How many messages a second is
  normal depends on the system; DCFD, a P25 system, sends about 25 a second. The card learns what
  is usual for your system and warns when the rate drops well under it.
- A few percent of control messages lost is normal. Over 10 % the card warns (multipath or a
  marginal signal), over 30 % it turns red.
- *Not decoding its control channel — hunting for another* means nothing is getting through.
  Check the antenna, the gain and the frequency, and that the control channel is inside a
  source's band.

Each source has a line too. It warns if the radio is dropping samples (the computer or USB can't
keep up), if samples hit full scale (lower the gain), or if its frequency is off by more than
2 ppm with AutoTune off. The **RF** page has the detail: each radio's noise floor, headroom,
frequency error and waterfall.

When calls start, they show on the **Calls** page: on the air now (press one to listen live) and
recently recorded. A call that wasn't recorded says why, for example *outside tuned range* when no
radio covers its voice frequency. The **Radio system** page counts these reasons over time; lots
of *Out of band* means you need another radio or a different center.

## Where recordings go

Calls are written to the recordings folder (**Recordings folder** on the **Recording** tab;
`~/TrunkRecorderPro` unless you chose another), in Trunk Recorder's layout:

```
<short name>/<year>/<month>/<day>/<talkgroup>-<start time>_<frequency>.wav
<short name>/<year>/<month>/<day>/<talkgroup>-<start time>_<frequency>.json
```

for example `dcfd/2026/10/6/101-1791320000_858237500.wav`. The JSON beside each call has the same
fields as Trunk Recorder's, so tools written for Trunk Recorder can read them. In the browser
version, calls are kept in the browser's storage instead and exported from **Recorded calls**.

File names, the call JSON, M4A copies and what is kept or deleted are covered in
[Recordings](guides/recordings.md).

Your settings are in `config.json` in:

| System | Folder |
|---|---|
| macOS | `~/Library/Application Support/trunk-pro/` |
| Windows | `%APPDATA%\trunk-pro\` |
| Linux | `~/.config/trunk-pro/` (or `$XDG_CONFIG_HOME/trunk-pro/`) |

`trunk-pro --config <file>` uses another config file; what the recorder learns and installs (band
plans, talker aliases, plugins) is then kept beside that file. See the
[configuration reference](configuration/README.md).

## Next steps

- Record more than one system or site: [Multi-site](guides/multi-site.md).
- Add more radios to catch every voice channel: [Radios](guides/radios.md).
- Upload calls to OpenMHz, Broadcastify or Rdio Scanner: [Plugins](plugins/README.md).
- Something not working: [Troubleshooting](troubleshooting.md).
