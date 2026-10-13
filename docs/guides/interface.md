# The web interface

Trunk Recorder Pro is run from your web browser. This page is a tour of the interface: the
frame around every page, Setup, the Listen page, the Live page, events, quitting, where the
history is kept, reaching it from another machine, and serving an interface of your own. The
measurement pages (Overview, RF, Decode, Radio system, Plugins, Platform) and what their numbers
mean are on [The dashboard](dashboard.md).

When you start `trunk-pro` it opens the interface at <http://localhost:8080>. If you closed the
tab, open that address again, or start the app again: a second copy notices the first one and
just opens the browser on it.

## The frame

On the left is the list of pages:

| Page | What it's for |
|---|---|
| **Overview** | Every system at a glance, today's numbers, the sources, the computer and the plugins in a line each, and what happened lately |
| **Listen** | A scanner over the recorded calls: hear them as they're recorded, pick the talkgroups, go back through the day |
| **RF** | How well the radios hear the air: noise floor, headroom, frequency error, dropped samples, the waterfall |
| **Decode** | How well signals turn into messages and voice, per system and per frequency |
| **Radio system** | The system as heard: talkgroups, radios, what happened to calls |
| **Live** | Calls on the air now (with live audio), the control channel log |
| **Plugins** | Each plugin's health, and installing and setting them up |
| **Platform** | The computer: CPU, memory, disks, network |
| **Setup** | Sources, systems, recording, the log, plugins |

Next to a page's name is a light, and hovering over it says why it is that colour:

- **green**: fine
- **amber**: worth a look (some control messages lost, a little clipping, a disk filling up)
- **red**: something is wrong (no control channel, dropped samples, a plugin that stopped)
- **no light** or grey: not recording, so nothing to judge

Across the top are:

- **Setup guide** (only while stopped): sets up a new system step by step. It opens by itself the
  first time, when the config is empty.
- the state: **Recording** (or **Replaying** when every source is a capture file), **Stopped**,
  **Starting…**, **Stopping…**, or **Disconnected** when the browser has lost the app.
- **Start** / **Stop**. If the settings can't start, **Start** is greyed out and hovering over it
  says why (for example "Add a source: a dongle or a capture file.").
- **Quit** (see [Quitting](#quitting)).

Banners under the top bar tell you about problems: `Not connected to trunk-pro. Retrying…` when
the app isn't answering, errors (with **Dismiss**), `Replay finished.` when a replay has run
through its captures, and, while stopped, what's missing from the setup with an **Open setup**
button.

Several browsers (or tabs, or machines) can have the interface open at once; they all see the
same recorder.

## Setup

**Setup** has five tabs; the number on each is how many of that thing you have:

| Tab | What's there | Details |
|---|---|---|
| **Systems** | **Find my system**, your trunked systems, **Add a system**, **Import Trunk Recorder config…** | [Find my system](find-my-system.md), [P25](p25.md), [SmartNet](smartnet.md), [DMR](dmr.md), [NXDN](nxdn.md) |
| **Conventional** | Conventional systems and their channels | [Conventional](conventional.md) |
| **Radios** | Your sources (dongles, other radios, capture files), **Add a source**, **Dongle not showing up?** | [Radios](radios.md) |
| **Recording** | **Recorder** (recordings folder, recorders, pre-roll, voice decoder, RAM spool, start on launch), **Call rules**, **Log**, **M4A audio**, **Interfaces** | [Recordings](recordings.md), [Interfaces of your own](#interfaces-of-your-own) |
| **Plugins** | Each plugin's settings | [Plugins](../plugins/README.md) |

There's no Save button. Every change is saved to the config file a moment after you make it.
Most changes take effect the next time you press **Start**. A few apply while recording:
talkgroup files (including the **Ignore** button on the Radio system page), the **Log**
settings, and plugin settings (the plugins restart with them).

If you're coming from Trunk Recorder, **Import Trunk Recorder config…** brings a `config.json`
over; see [Migrating from Trunk Recorder](../migrating-from-trunk-recorder.md). Anything the
import couldn't finish shows as a to-do list at the top of Setup, with a count on the tab it
belongs to.

## Listen

Listen works like a scanner app (rdio-scanner and its kin): each call plays once it's recorded,
one after another. On the left is the display and its buttons, on the right the recent calls and
the talkgroups to hear.

- **Live feed**: on, each new call you've chosen is queued and played in turn; `Q` is how many
  are waiting. Off, nothing new plays. A call is heard once it ends, so the feed runs a call's
  length behind the air. Turning it on also turns off **Listen live** on the Live page (one sound
  at a time).
- **Hold sys** / **Hold TG**: only the current call's system, or its talkgroup, plays until you
  press it again.
- **Pause** stops the audio and the queue keeps filling; **Replay** plays the current call from
  the start (or the last one again); **Skip** goes to the next.
- **Avoid** stops playing the current talkgroup until you press it again; **30 / 60 / 120 min**
  avoids it for that long.
- **Save** downloads the call's audio.

The display shows the talkgroup's name, its description, tag and category from the talkgroup
file, the system, the talkgroup ID, the frequency, the first radio to speak and how far into the
call it is (click the bar to jump). **Recent** under the buttons is the last five calls played;
click one to hear it again.

Encrypted calls and calls whose audio wasn't kept (deleted after uploading) are left out. A call
kept as M4A plays the M4A, else the WAV.

### Calls

The calls recorded, newest first: the 300 newest on disk when the app started, then each new
one. Click a call to play it now; the feed goes on after it. **Selected talkgroups only** hides the
calls you wouldn't hear. **Load older** reads further back through the recordings folder, 200
calls at a time, as far as 24 hours ago.

Load older reads the day folders of the usual layout (`<system>/<year>/<month>/<day>/`): calls
saved with a [filename format](recordings.md) of your own aren't found.

### Talkgroups

What the feed plays: every system and talkgroup, from the talkgroup files and what's been heard.
Untick a system or a talkgroup to stop hearing it; the **Groups** and **Tags** chips (the
talkgroup file's Category and Tag) switch all of theirs at once, and a dashed chip means some are
on. **All on** and **All off** do everything. Talkgroups heard but not in a file are on until you
turn them off.

What you pick, holds and avoids are kept in this browser, so another browser (or another
computer) has its own.

## Live

The Live page has two parts.

### Active calls

Every call on the air now, recording ones first:

| Column | What it shows |
|---|---|
| **System** | Only with more than one system |
| **Talkgroup** | Its number and name, `EMERG` for an emergency call, what it's patched with, and `also on …` when the same call is on other sites (the best copy is saved) |
| **Freq, MHz** | The voice channel, with `s0`/`s1` for a TDMA slot, `FM` (and its tone) for analog, `NXDN48`/`NXDN96` and the RAN for NXDN |
| **Source** | The radio talking now: its name or talker alias when known, else its unit ID |
| **Time** | How long the call has gone on |
| **State** | `recording`, `recording (encrypted)`, or why it isn't being recorded |

The reasons a call isn't recorded:

| State | Why | What to do |
|---|---|---|
| `encrypted — not recorded` | The call is encrypted and encrypted calls are off | Nothing: encrypted audio can't be decoded |
| `outside tuned range` | No source covers its voice frequency | Move a source's centre, or add a source. See [Radios](radios.md). |
| `no free recorder` | Every recorder is busy | Raise **Recorders** in Setup → Recording |
| `not in talkgroup list` | It's not in the talkgroup file and **Talkgroups not in the talkgroup file** is off | Add it, or turn that on in Call rules |
| `ignored (talkgroup list)` | It's set to Ignore in the talkgroup file | Un-ignore it on the Radio system page |
| `monitoring` | Followed, not recorded, for another reason | |

With several systems, chips above the table (**All**, then each system) show one system's calls.

### Listening live

Tick **Listen live** to hear calls as they're decoded, like a scanner. It stays on one call
until that call has been quiet for 2 seconds, then takes the next one. The line under the
heading shows what's playing (`▶ TG 3747 · <radio>`).

- **The system chips filter what you hear too**: pick a system and only its calls play.
- The **Listen** button on a recording call plays only that talkgroup. A button then appears
  next to **Listen live** (`TG 3747 only ✕`); click it to go back to everything.
- Only calls being recorded can be heard.
- The audio comes from the recorder over the same connection as the rest of the page, so it works
  from another machine too.
- Safari can pause the audio when you switch tabs or audio devices; a click or key press anywhere
  on the page starts it again.

### Control channel log

Click **Control channel log** to open it. It shows the last 150 control channel messages as they
arrive, newest first: grants, updates, patches, the system's identity, neighbouring sites, talker
aliases, protocol notes (such as a DMR site's colour code and kind), plugin notes and errors. Tick **Show unit activity** to also see affiliations,
registrations and the like. With several systems, chips pick one.

The log is only made while it's open, because turning every control message into a line costs
the recorder some work. This is not the same as the app's log file: see
[Log levels and log files](../troubleshooting.md#where-is-the-log).

## Events

The **Lately** card on the Overview lists notable things as they happen, newest first, with a
coloured light. Click one to go to the page about it. These are the events you can see:

| Event | Colour | What it means, and what to do |
|---|---|---|
| `<system>: talkgroup … heard for the first time` | info | A talkgroup you hadn't heard before. Worth a look on the Radio system page; add it to your talkgroup file if you want a name for it. |
| `<system>: radio … heard for the first time` | info | A new radio ID. Only reported once the app has a baseline for the system. |
| `<system>: lost its control channel (… MHz)` | red | The control channel stopped decoding and the recorder is hunting for another. Check the antenna, gain and frequency; see [Troubleshooting](../troubleshooting.md#the-control-channel-isnt-decoding). |
| `<system>: decoding its control channel again` | green | Back to normal. |
| `<source>: dropping samples` | amber | The computer or USB bus isn't keeping up; calls on that source lose audio. See [Troubleshooting](../troubleshooting.md#dropped-samples). |
| `<source>: clipping (…% of samples at full scale)` | amber | More than 0.5% of samples are at full scale: the gain is too high, or a strong signal is nearby. Lower the gain a few dB. |
| `<source>: clean again` | green | No more drops or clipping. |
| `<plugin>: error — …` / `warning — …` / `ok — …` | red / amber / green | A plugin's own status changed. See the Plugins page. |
| `<target> stopped answering` | red | One of the internet checks (`monitor.probeHosts`) failed. Your uploads probably aren't getting through either. |
| `<target> answers again (down … s)` | green | It's back. |
| `The <name> disk has only …% free` | red | The recordings or app data disk is under 5% free. Free some space, or turn on M4A and the clean-up options in [Recordings](recordings.md). |
| `The RAM spool has only …% free: uploads aren't keeping up` | amber | The RAM spool is under 25% free. Check the plugins. |
| `The RAM spool has only …% free: calls will soon go to the disk` | red | Under 10% free. |
| `The RAM spool was full: … calls written to the recordings folder instead` | red | Nothing was lost; those calls went to disk. Make the spool bigger, or find out why uploads are slow. |
| `The RAM spool has room again (…% free)` | green | Back to normal. |

The app keeps the last 200 events in memory, so a browser that opens later still sees them; they
are not kept when the app quits. There are no alerts (email, push) yet: events only show in the
interface.

## Quitting

**Quit** stops recording (calls in progress are saved), stops the app, and shows
`Trunk Recorder Pro has quit`. If it's recording, it asks first. Ctrl-C in the terminal and
`SIGTERM` (`systemctl --user stop trunk-pro`) do the same. On macOS the app has no Dock icon, so
**Quit** is how you stop it; on Windows, closing the console window does the same.

To start recording again after a restart without touching anything, turn on **Start recording
when the app starts** in Setup → Recording (or start with `--start`).

## Where the history is kept

The dashboard's charts can go back a week, across restarts. The app keeps, in the folder its
config file is in:

| Folder | What | Kept |
|---|---|---|
| `stats/` | Each minute's measurements, one file a day (`YYYY-MM-DD.jsonl`, UTC days). A finished day is gzipped. | 8 days (a week and today); older files are deleted |
| `radio/` | What each system's talkgroups and radios have done, hour by hour (`<system>.json`), for the Radio system page. Saved every 15 minutes and when the app quits. | a week of hours |

Files are only appended to, never rewritten, which is kind to SD cards. Deleting `stats/` while
the app is stopped clears the charts' history and nothing else.

## Remote access

By default the interface only answers on this computer (`127.0.0.1`). To reach it from another
machine on your network, start with `--bind 0.0.0.0`, or set `server.bind` in the config (see
[Server and log](../configuration/server-and-log.md)):

```bash
trunk-pro serve --bind 0.0.0.0 --no-open
```

Then browse to `http://<that machine's address>:8080`.

**There is no login.** Anyone who can reach the port can change the settings, start and stop
recording, quit the app, listen, and download your recordings. Only do this on a network you
trust, never on a port open to the internet.

A safer way, from a machine that can SSH to the recorder, is an SSH tunnel. Leave the recorder
bound to `127.0.0.1`, then on your machine:

```bash
ssh -L 8080:localhost:8080 you@recorder.local
```

and open <http://localhost:8080>. The interface is now reachable only through your SSH login.

### Why a page gets "Refused"

The app checks every request a browser sends, so that some other web page you happen to have open
can't drive the recorder behind your back:

- A web page from another origin (a custom interface served somewhere else, or a page opened from
  a file) is refused with
  `Refused a page from <origin>: add it to server.allowedOrigins in the config to let it use the interface.`
- While bound to `127.0.0.1`, a request that arrives under any name other than `localhost` (or a
  loopback address) is refused with `Refused a request for <host>: this server only answers to
  localhost …`. This blocks DNS rebinding.

To allow a page, add its origin to **Other pages allowed to use the API** in Setup → Recording →
Interfaces (`server.allowedOrigins`), one per line, as the browser sends it
(`http://192.168.1.50:3000`). `null` allows pages opened from a file, and `*` allows any page:
only on a private network. Programs that aren't browsers (scripts, `curl`) aren't affected by any
of this.

## Interfaces of your own

You can replace or add to the built-in interface with web pages of your own: a scanner page, a
wall display, a talkgroup monitor. The recorder serves them and they talk to it over the same API
the built-in interface uses.

In Setup → Recording → **Interfaces**:

1. **Add an interface**, give it a **Name** (letters, digits, `-` and `_`) and a **Folder** with an
   `index.html` in it (absolute, `~/…`, or relative to the config file's folder).
2. It's served at `/ui/<name>/`. **All interfaces** (`/ui/`) lists yours along with the examples.
3. **Show at /** picks what <http://localhost:8080/> shows: **This interface** (the built-in one)
   or one of yours. The built-in interface is always at `/builtin/`.

From the command line, `--ui <folder>` serves a folder at `/` for one run without changing the
config. These are kept in `server.interfaces` and `server.home`.

How to build one (the API, its messages, examples and a JavaScript client) is in
[docs/api](../api/README.md); a running recorder serves the same guide at `/api/docs`.

## In the browser version

The browser version (no install) has the same pages except **Plugins** and **Platform**, and no
**Quit**: it records in the tab, keeping the screen awake while it does, and keeps calls in the
browser. See [In the browser](../install/browser.md).
