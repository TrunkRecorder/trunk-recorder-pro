# The dashboard

The dashboard pages show how the recorder is doing: whether each system's control channel is
decoding as usual, how well the radios hear the air, how clean the voice is, what the system is
doing, and whether the computer and plugins are keeping up. This page goes through each one and
what its numbers mean. For the frame around them, Setup and the Listen page, see
[The web interface](interface.md).

Most numbers come with a small chart of the last 10 minutes, and many pages have an **Over time**
section with a **Span** of **1 h**, **24 h** or **7 days**. The history survives restarts (see
[Where the history is kept](interface.md#where-the-history-is-kept)). While stopped, the pages
show past data where they can.

Several numbers are compared with **the usual**: that series' average over the last 24 hours.
The shaded band on a chart is its usual range, and an arrow shows how far today is above or below
it. This is how the dashboard tells "the system is quiet tonight" from "reception got worse".

## Overview

While recording, the top row is today's big numbers ("today" is since local midnight, across
restarts):

| Number | What it is |
|---|---|
| **Calls today** | Calls saved today, with how many an hour right now and the last hour against the usual |
| **Airtime today** | Total length of today's calls, and how many are on the air at once on average |
| **Audio today** | Size of today's audio, and how much an hour right now |
| **Recorders** | Calls being recorded now, of **Recorders** in Setup, and how many channels are open. Amber when they're all busy. |
| **Missed, 24 h** | Calls not recorded because no recorder was free or no source covered the frequency. Click to see the Radio system page. |
| **Decoding load** | How much of one CPU core decoding takes. Over 100% means it's using more than one core, which is fine on a multi-core computer. |

Below that is a card for each system. Click its name to go to its Decode page.

- **Control channel** (trunked systems): control messages decoded a second, against the usual;
  the share decoded; the control channel's frequency (or `hunting` while it looks for one); and
  its SNR.
- **calls/min**, **on the air** now, **voice lost** (the share of voice frames that couldn't be
  decoded over the last 10 minutes; under 2% is clean), and **new TGs** (first heard in the last
  24 hours).
- Chips for the talkgroups on the air; click one for its page.
- **voice quality by frequency, 24 h**: one cell per voice channel, darker where more voice was
  lost. A dark cell or two at one end often means a source's band edge.

On the right are the **Sources** (as on the RF page, smaller), then **Status**: a **Computer**
line (CPU, memory, disk, RAM spool, internet), a **Plugins** line (sent, failed, queued for each
plugin that's on), and **Lately**, the event feed (see [Events](interface.md#events)).

## RF

How well each radio hears the air. While stopped, it lists your sources' settings.

**The band** draws every source's passband on one axis, with the control channels, conventional
channels and calls (recording and followed-only) marked on it. The darker part of each passband
is what the source actually uses, its sample rate less the guard band at each edge. Use it to see whether your
sources cover what they should.

Each source then has a card:

| Number | What it means |
|---|---|
| **Noise floor**, dBFS | The median power across the band. It rises with gain, and with interference. A sudden rise with no gain change means something nearby started making noise. |
| **Headroom**, dB | How far the strongest signals are below full scale (the point where the radio clips). Under the meter: the share of samples clipping, if any. |
| **Frequency error**, ppm | How far off the radio's frequency is, measured against the control channels (the towers' frequencies are very accurate). With AutoTune on it's corrected as it's measured; otherwise it says `not corrected (AutoTune off)`. It needs a control channel to measure. Steady: set the source's ppm. Drifting: temperature. See [Radios](radios.md). |
| **Dropped**, samples/s | Samples the driver lost because the computer or the USB bus didn't keep up. Anything but zero is red: calls on that source lose audio. |

Under the card: the gain as set, the sample rate actually delivered against the one asked for,
and the source's errors, if any (a USB error, a dongle that went away).

The **Waterfall** beside each source shows its whole band over time, with the control channels
and active calls marked. **Show** turns it off; the browser remembers that, and the recorder
stops sending that spectrum.

Click a source's name for its own page, which adds:

- **Across the band**: the noise floor across the band (bars) and each channel's power above it
  (stems). 10 dB or more above the floor is clean. A floor that sags at the edges is the radio's
  roll-off; keep channels out of it (see the guard band in [Radios](radios.md)).
- **Channels on this source**: each channel's power, floor, **SNR** (green at 15 dB or more, amber
  from 8, red below), **Offset** (how far its carrier is from where it should be, in Hz) and
  **Quality** (see [Decode](#decode)). If every carrier is off by about the same ppm, it says so:
  set the source's ppm or turn on AutoTune.
- **Over time**: noise floor and peaks, frequency error (measured and corrected), dropped samples,
  clipping.

## Decode

How well signals turn into messages and voice. A card per system:

| Number | What it means |
|---|---|
| **Control messages** /s | Control channel messages decoded a second, against the usual. How many there should be depends on the system: compare with its own usual. |
| **Control lost** % | Messages that didn't decode. A few percent is normal; amber over 10%, red over 30%. |
| **Coverage** % | P25: an estimate from timing of how much of the control channel decoded, against the most it could carry. Unlike messages a second it reads the same on every system: a multi-block message takes two or three TSBKs' air. Frames never caught count against it. 96–98% is healthy. |
| **Eye opening** | C4FM (P25 Phase 1 and most control channels): how well separated the four symbol levels are. 10 or more is clean, under 4 means errors, about 1 is noise. Simulcast systems read low even when they decode well. |
| **Phase error**, ° rms | CQPSK (simulcast P25, and Phase 2): how far symbols land from where they should. Under 10° is clean, amber from 15°, red from 20°, about 25° is noise. |
| **Deviation**, Hz | SmartNet: how far the signal swings. About ±2.4 kHz is normal; less means weak or filtered. |
| **carrier** (under these) | The control channel's carrier offset in Hz. A steady offset is the radio's frequency error. |
| **Framing**, % missed | P25: frames whose header (NID) failed or whose sync was missed, and how often the equaliser reset a minute |
| **Voice lost**, % of frames | Voice frames too damaged to use, over 10 minutes. Under 2% is clean, amber from 2%, red from 8%. Under it: bit errors the error correction fixed per frame. |

Each card also has a **voice lost by frequency, 24 h** strip.

Click a system's name for its own page, which adds:

- its identity (NAC, WACN, SysID, site) and calls saved this run;
- **Channels now**: each open channel's SNR, offset and quality;
- **Over time**: decoded and lost control messages, control channel coverage (P25), voice frames
  lost, SNR with eye opening or phase error, carrier offset;
- **Frequencies, 24 h** (or 7 days): every voice frequency with calls, its source, **In band**
  (0% at the edge of the source's usable band, guard band left out, 100% at its centre), calls, SNR, offset, the share of voice
  frames decoded cleanly and lost, and an hour-by-hour strip. Above the table the page points out
  patterns: every frequency off by the same ppm, a frequency at a band edge decoding worse ("move
  the centre toward it"), or one frequency much worse than the rest (interference or a weak
  path);
- neighbouring sites, patches, and for DMR and NXDN the site's channels.

## Radio system

The system as heard. With several systems it first shows a card each (talkgroups and radios in
24 hours, new talkgroups, talkgroups not in its file, the busiest talkgroups, what's on the air);
clicking a system's card goes to it. The **Span** is 24 hours or 7 days.

| Number | What it is |
|---|---|
| **Calls**, **Airtime** | In the span, with the average call length |
| **Talkgroups heard** | With how many are in your talkgroup file |
| **Radios heard** | With how many in the last hour |
| **New talkgroups** | First heard in the last 24 hours (`learning (first hour)` while it builds a baseline) |
| **Unknown** | Heard in 24 hours but not in the talkgroup file |

**What happened to calls** splits the calls by what became of them: **Recorded**, **Followed
only**, **Ignored**, **Encrypted**, **Not in the file**, **No recorder** and **Out of band**. The
last two are calls you missed; see the reasons table under [Active calls](#active-calls).

Below the numbers: **Active calls** (see below), **What happened to calls** and **Call lengths**,
**Busiest … by hour**, **Who talks on what** (talkgroups and the busiest radios, linked by how
often each radio talks on each), the **Talkgroups** table, the **Radios** table, and at the
bottom the **Control channel log**.

The **Talkgroups** table can show **All**, **New**, **Unknown**, **Ignored** or **Encrypted** (more
than half their calls encrypted), sorted by **Airtime**, **Calls**, **Latest** or **Newest**. The
**Ignore** button stops recording a talkgroup by marking it Ignore in the system's talkgroup file;
it takes effect at once, even while recording. Click it again (**Ignored ✕**) to record it again.

Click a talkgroup or a radio anywhere for its own page: its last 7 days, who talks on it (or what
it talks on), what's affiliated with it, and its latest transmissions.

### Active calls

Near the top of a system's page: every call of that system on the air now, recording ones first. It
replaces the small "On the air now" card, and the old Live page (`#/live` and `#/calls` links open
the Radio system page).

| Column | What it shows |
|---|---|
| **Talkgroup** | Its number and name (click it for the talkgroup's page), `EMERG` for an emergency call, what it's patched with, `· <site>` for a call heard on another site of the system, and `also on …` when the same call is on other sites (the best copy is saved) |
| **Freq, MHz** | The voice channel, with `s0`/`s1` for a TDMA slot, `FM` (and its tone) for analog, `NXDN48`/`NXDN96` and the RAN for NXDN |
| **Source** | The radio talking now: its name or talker alias when known, else its unit ID. Click it for the radio's page. |
| **Time** | How long the call has gone on |
| **State** | `recording`, `recording (encrypted)`, or why it isn't being recorded |
| (button) | **Listen** plays just that talkgroup |

The reasons a call isn't recorded:

| State | Why | What to do |
|---|---|---|
| `encrypted — not recorded` | The call is encrypted and encrypted calls are off | Nothing: encrypted audio can't be decoded |
| `outside tuned range` | No source covers its voice frequency | Move a source's centre, or add a source. See [Radios](radios.md). |
| `no free recorder` | Every recorder is busy | Raise **Recorders** in Setup → Recording |
| `not in talkgroup list` | It's not in the talkgroup file and **Talkgroups not in the talkgroup file** is off | Add it, or turn that on in Call rules |
| `ignored (talkgroup list)` | It's set to Ignore in the talkgroup file | Un-ignore it on the Radio system page |
| `monitoring` | Followed, not recorded, for another reason | |

### Listening live

Tick **Listen live** to hear that system's calls as they're decoded, like a scanner. It stays on
one call until that call has been quiet for 2 seconds, then takes the next one. The line under the
heading shows what's playing (`▶ TG 3747 · <radio>`).

- Live audio is one system at a time: on another system's page it says "Listening to <system>".
- Turning on Listen's **Live feed** turns **Listen live** off, and the other way round (one sound
  at a time).
- The **Listen** button on a recording call plays only that talkgroup. A button then appears
  next to **Listen live** (`TG 3747 only ✕`); click it to go back to everything.
- Only calls being recorded can be heard.
- The audio comes from the recorder over the same connection as the rest of the page, so it works
  from another machine too.
- Safari can pause the audio when you switch tabs or audio devices; a click or key press anywhere
  on the page starts it again.

### Control channel log

At the bottom of a system's page, collapsed. Click **Control channel log** to open it. It shows that
system's last 150 control channel messages as they arrive, newest first (a site's name in brackets
for a system with several sites): grants, updates, patches, the system's identity, neighbouring
sites, talker aliases, protocol notes (such as a DMR site's colour code and kind), plugin notes and
errors. Tick **Show unit activity** to also see affiliations, registrations and the like.

The log is only made while it's open, because turning every control message into a line costs
the recorder some work. This is not the same as the app's log file: see
[Log levels and log files](../troubleshooting.md#where-is-the-log).

## Plugins

The **Health** tab has a card for each plugin that's on (**Install & settings** is covered in
[Plugins](../plugins/README.md)):

| Number | What it is |
|---|---|
| **Sent** | Calls it handled this run, and how many it skipped |
| **Failed** | Calls it failed on, as a share of tries. Hover for the last failure. |
| **Waiting** | Calls queued for it (amber over 20), and how many are waiting to retry |
| **Age at delivery** | How long after a call ended the plugin finished with it, over the last 5 minutes (or the hour, when nothing was delivered in the last 5), and its parts: **held** by your delay rules, **waiting** (the call's files being finished, the plugin's queue, failed tries) and the plugin's own **upload**. Then the hour's longest. A plugin built before SDK 0.6 doesn't say its upload time, so only the age shows. All by this computer's clock: no service reports it. |

Under them: how much it has sent this run, a bar per minute for the last hour (sent, skipped, failed), each service the plugin
talks to with its state, any figures of its own the plugin reports (simplestream's packets sent and
dropped, for example), and when it last succeeded and failed. **Restarted N×** means the plugin
crashed and was restarted; **Fell behind N events** means it was too slow to take events and some
were dropped. **Last 24 h and log ▾** shows its history (the age at delivery among it) and recent log lines.

The age at delivery is local, so it's only as right as this computer's clock: see [Clock](#clock).
A call recorded while the clock was off says so in its JSON (`"clock_off": true`).

A service shows as down after 3 failures in a row, or 5 minutes without a success.

## Platform

The computer the recorder runs on, recording or not:

| Number | What it is |
|---|---|
| **CPU** | All cores together (of the container's limit, in a container). Amber over 90%. |
| **Trunk Recorder Pro** | The app's own CPU use, in cores, and its memory |
| **Memory** | Memory in use, and how much of the time programs stall waiting for memory (Linux) or the system's memory pressure (macOS) |
| **Internet** | Whether the internet checks answer, and how long a connection takes |
| **Temperature** | The hottest sensor, and whether a Raspberry Pi is throttling (under-voltage or heat) |

**Over time** has CPU (with each core) and memory. **Disks** shows the **Recordings** disk, the
**App data** disk and the **RAM spool** if it's on, with free space, calls that overflowed from
the spool, and **About … of recordings left** at last week's daily average. **Network** shows each
internet check (`monitor.probeHosts`, by default `1.1.1.1:443` and `dns.google:443`), and **This
computer** the system, cores, uptime and anything the platform can't measure.

### Clock

Call times come from this computer's clock. Calls from several recorders are matched by their
times, and services such as Broadcastify Calls go by them, so the clock is worth watching. The
recorder only watches it: the operating system's time service (chrony, systemd-timesyncd, ntpd,
macOS) keeps it. Nothing extra needs installing, and nothing runs with privileges.

| Number | What it is |
|---|---|
| **Offset, measured** | This clock less the time server's (`monitor.timeServer`, by default `pool.ntp.org`), from one SNTP query every 15 minutes, and whether this clock is behind or ahead. It's measured here, so it's right whatever the time service says. |
| **Operating system** | Whether the time service tells the kernel the clock is synchronized, and its own bound on the error. The bound is loose: a synchronized clock often claims hundreds of milliseconds. Linux and macOS; on Windows only the offset is measured. In a container this is the host's clock, which the container shares. |
| **Round trip** | The query's round trip. The offset is good to about half of it. |

Under them, when they're known:
- **Kept by…**: the time service and the server it follows, its stratum and when it last
  synchronized (Linux: chrony or systemd-timesyncd).
- **Calls are stamped by the clock as it was at Start**: while recording. Calls are stamped by the
  clock as it was when recording started, carried on by a clock that never jumps, so a call's times
  never go backwards. A correction the time service makes after Start reaches them only at the
  next Start: this line says how far apart the two are now.
- **The radio systems' clocks**: P25 systems (Phase 2) broadcast their own time, to 7.5 ms,
  usually from GPS. Each site's line says how far call times are from it. Some systems mark their
  time not valid even when it's right (DCFD does): those are shown, not gone by. A system set to its
  local time is far off by hours and isn't compared.

The chart shows the offset over time. The Platform light turns amber, with an event, when call
times are off by `monitor.clockWarnMs` (250 ms) or more, or the operating system says the clock
isn't synchronized; red at 5 s. What's judged is the clock calls are stamped by:
- measured against the time server, as the clock now plus how far it has moved since Start;
- without an answer from the time server, against the radio systems that vouch for their time
  (offline recorders).

A measurement over an hour old isn't counted. A jump of the clock (its time service setting it, or
the computer sleeping) is an event of its own. Some networks block NTP (UDP port 123): then nothing
is measured, the card says the queries go unanswered, and the internet checks don't count it.
Pointing `monitor.timeServer` at a server inside your network (often your router) can help.

The offset is local: nothing Broadcastify or another service reports. Broadcastify Calls doesn't
report how far off a recorder's clock is.

On macOS, if Spotlight is indexing the recordings folder, a note at the top explains how to
exclude it. See [Troubleshooting](../troubleshooting.md#spotlight).
