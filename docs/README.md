# All About Trunk Recorder Pro

Trunk Recorder Pro records the calls on trunked and conventional radio systems. It uses one or more
Software Defined Radios (SDRs) to capture a wide slice of spectrum, follows the system's control
channel, and records every call it can hear as a WAV file with a Trunk Recorder–compatible JSON file
beside it. Plugins can then upload those calls to OpenMHz, Broadcastify Calls or Rdio Scanner,
stream them, or hand them to a script of your own.

It is a rewrite of [Trunk Recorder](https://github.com/TrunkRecorder/trunk-recorder) in Rust. All
of the signal processing and decoding is built in: there is no GNU Radio or OP25 to install. It
is a single program, a few megabytes in size. You set it up and watch it from a browser
interface, which it serves itself. The same engine also runs entirely inside a web browser, with
nothing to install.

Trunk Recorder Pro supports:

- **P25** trunked systems: Phase 1 (C4FM and CQPSK / simulcast) control channels, with Phase 1 and
  Phase 2 (TDMA) voice
- **SmartNet / SmartZone** trunked systems: 800 MHz, 900 MHz and VHF / UHF (OBT) band plans, analog
  and digital voice
- **Trunked DMR**: Motorola Capacity Plus (and Linked Capacity Plus), Capacity Max, Connect Plus
  and ETSI Tier III
- **NXDN** (NXDN48 and NXDN96): Type-C and Type-D trunking. NXDN has been tested on recordings and
  synthesized signals, but not yet on a live trunked system.
- **Conventional channels**: analog FM, P25, DMR and NXDN, on their own or alongside trunked
  systems. Several users of one frequency can be told apart by CTCSS tone, DCS code, NAC, colour
  code or RAN.
- Several systems, and several sites of one system, at the same time. When a call is heard on more
  than one site, the best copy is kept.

Supported platforms: **macOS** 11 or newer (Apple silicon and Intel), **Linux** (x86-64 and
ARM64, glibc 2.28 or newer, including the **Raspberry Pi** 4 and 5 with a 64-bit OS),
**Windows** 10 and 11 (64-bit), **Docker** on Linux, and **Chrome or Edge** for the browser
version.

...and SDRs: RTL-SDR dongles (R820T / R828D tuners natively, older tuners through SoapySDR), Ettus
USRPs through UHD, Airspy R2 and Mini, and anything else with a SoapySDR module (HackRF, SDRplay,
LimeSDR...).

## Install

- [Choosing how to run it](install/README.md), and building from source
- [macOS](install/macos.md)
- [Windows](install/windows.md)
- [Linux](install/linux.md)
- [Raspberry Pi](install/raspberry-pi.md)
- [Docker](install/docker.md)
- [In the browser](install/browser.md), with nothing to install
- [USRP, Airspy and SoapySDR radios](install/other-radios.md)

## Setup

- [Getting started](getting-started.md): from a new install to your first recorded call
- [Radios](guides/radios.md): center frequency, sample rate, gain, frequency correction, using
  more than one SDR
- [Find my system](guides/find-my-system.md): let the recorder scan for control channels and set
  itself up

### Radio systems

- [P25](guides/p25.md)
- [SmartNet](guides/smartnet.md)
- [Trunked DMR](guides/dmr.md)
- [NXDN](guides/nxdn.md)
- [Several systems and sites](guides/multi-site.md)
- [Talkgroups and unit names](guides/talkgroups-and-units.md)
- [Conventional channels](guides/conventional.md)
- [Tones, NACs and colour codes](guides/tones.md): splitting a shared frequency

### Using it

- [The interface](guides/interface.md): Setup, live listening, events, remote access
- [The dashboard](guides/dashboard.md): what each page and number means
- [Recordings](guides/recordings.md): where calls go, file names, the call JSON, what is kept
- [Plugins](plugins/README.md): uploading to [OpenMHz](plugins/openmhz.md),
  [Broadcastify](plugins/broadcastify.md) and [Rdio Scanner](plugins/rdioscanner.md),
  [streaming](plugins/simplestream.md), [running a script](plugins/upload-script.md)
- [Coming from Trunk Recorder](migrating-from-trunk-recorder.md): importing your config, and what
  is different
- [Troubleshooting](troubleshooting.md)

## Reference

- [The config file](configuration/README.md):
  [sources](configuration/sources.md),
  [trunked systems](configuration/systems.md),
  [conventional systems](configuration/conventional.md),
  [recording](configuration/recording.md),
  [server and log](configuration/server-and-log.md),
  [plugins](configuration/plugins.md)
- [Command line](command-line.md)

## For developers

- [Development](development.md): building from source, making a release, the code layout and how
  it was verified
- [Architecture](architecture.md): how it works, from SDR to audio file
- [Performance](performance.md): CPU use measured against Trunk Recorder
- [Building your own interface](api/README.md): the WebSocket API the built-in interface uses
- [Writing a plugin](plugins/writing-plugins.md)

## How Trunking Works

For those not familiar, trunking systems allow a large number of user groups to share a limited
number of radio frequencies by assigning frequencies to talkgroups (channels) on demand. Most
user groups actually use the radio very sporadically and don't need a frequency of their own.

Most trunking systems (P25, SmartNet, Capacity Max, NXDN Type-C) set aside one frequency as a
**control channel** that manages and broadcasts frequency assignments. When someone presses the
Push to Talk button on their radio, the radio asks the system for a channel. The system assigns
a voice frequency and broadcasts a **grant** about it on the control channel. This tells the
radio which frequency to transmit on, and tells the other radios on that talkgroup to listen.

To follow every conversation, Trunk Recorder Pro constantly listens to and decodes the control
channel. When a grant arrives, it opens a voice channel on that frequency, out of the spectrum the
SDR is already capturing. It keeps a second of recent spectrum in memory, so the channel starts
with the air from just *before* the grant and the first syllable isn't lost. Nothing is sent on
the control channel when a conversation is over, so the recorder ends the call when nothing has
been heard on it for a few seconds (the call timeout, 3 seconds by default).

Some systems have no control channel. On Capacity Plus and NXDN Type-D, each repeater says on its
own channel who is talking on it, so the recorder watches every repeater of the site at once.
Conventional channels have no trunking at all: each user group has its own frequency, and the
recorder opens a channel whenever a signal appears on one.
