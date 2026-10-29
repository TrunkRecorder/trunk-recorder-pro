# Trunk Recorder Pro API — build your own interface

Everything Trunk Recorder Pro's built-in web interface does goes through the
API described here, so an interface of your own can do all of it: a
scanner-style listening page, a wall display, a phone view, a dashboard for
one talkgroup, a program that logs calls or forwards them somewhere.

**Two guides:**
- This page is for **developers**: how to serve an interface, the client
  library, the examples, and every message.
- [`llms.txt`](llms.txt) is for an **LLM** building an interface for you:
  the same facts, condensed, with rules and a template. See
  [Having an LLM build an interface](#having-an-llm-build-an-interface).

[`protocol.ts`](../../web/src/protocol.ts) is the exact definition of every
message. Both guides defer to it.

A running recorder serves all of this, on the built-in interface's port and on
every other interface's ([Serving your interface](#serving-your-interface)):

| URL | What |
|---|---|
| `/` | The interface this port is for: the built-in one on the recorder's port (8080), yours on its own |
| `GET /api/docs` | This page (Markdown) |
| `GET /api/llms.txt` | The LLM guide |
| `GET /api/client.js` | The client library ([client.js](#clientjs)) |
| `GET /api/examples/…` | The examples, runnable: e.g. `/api/examples/wall.html` |
| `GET /api/protocol.ts` | Every message, as TypeScript types with comments: **the definition** |
| `GET /api/schema` | The same as JSON Schema (draft-07): `definitions.FromRecorder` / `definitions.ToRecorder` |
| `GET /api/version` | `{"app": "trunk-pro", "version": "…", "api": 1}` (`api`: the interface API version) |
| `WS /api/ws` | The connection: JSON messages both ways, plus binary live audio |
| `GET /calls/<path>.wav` / `.json` | A recorded call's audio and details |

The recorder listens on `http://localhost:8080` unless it was started with
`--port` / `--bind` or its config's `server` section says otherwise.

## Contents

1. [Quick start](#quick-start)
2. [Serving your interface](#serving-your-interface)
3. [client.js](#clientjs)
4. [Examples](#examples)
5. [Access from elsewhere](#access-from-elsewhere)
6. [How a connection goes](#how-a-connection-goes)
7. [Concepts](#concepts)
8. [Messages from the recorder](#messages-from-the-recorder)
9. [Commands to the recorder](#commands-to-the-recorder)
10. [Live audio](#live-audio)
11. [Recorded calls](#recorded-calls)
12. [Recipes](#recipes)
13. [Having an LLM build an interface](#having-an-llm-build-an-interface)
14. [Statistics and history](#statistics-and-history)
15. [Compatibility](#compatibility)

## Quick start

Make a folder with this `index.html`:

```html
<!doctype html>
<meta charset="utf-8">
<title>Calls</title>
<button id="listen">Listen live</button>
<ul id="calls"></ul>
<script type="module">
  import { TrunkClient, LivePlayer } from "/api/client.js";

  const rec = new TrunkClient();
  const player = new LivePlayer();

  rec.on("change", () => {
    document.getElementById("calls").innerHTML = rec.history
      .slice(0, 20)
      .map((e) => `<li>${e.record.talkgroup_tag || e.record.talkgroup}: ${e.record.call_length} s
                   <audio controls preload="none" src="${rec.callUrl(e)}"></audio></li>`)
      .join("");
  });

  document.getElementById("listen").onclick = async () => {
    await player.resume(); // browsers only play sound after a click
    rec.listen(); // every call; rec.listen({ talkgroup: 101 }) for one
  };
  rec.on("audio", (chunk) => player.play(chunk));
</script>
```

Then serve it from the recorder:

```bash
trunk-pro --ui ./my-folder      # your page on the next free port, http://localhost:8081/
```

Edit the file and reload: the recorder serves the folder as it is on disk.

Without the library, the protocol is plain JSON over a WebSocket. This prints
each call as it's recorded, in Node 22+ or a browser:

```js
const ws = new WebSocket("ws://localhost:8080/api/ws");
ws.onmessage = (ev) => {
  if (typeof ev.data !== "string") return; // live audio (binary), not asked for here
  const m = JSON.parse(ev.data);
  if (m.type === "hello") console.log(`Connected to ${m.version}`);
  if (m.type === "concluded") {
    const r = m.entry.record;
    console.log(`${r.talkgroup_tag || r.talkgroup} on ${r.short_name}: ${r.call_length} s`);
    console.log(`  audio: http://localhost:8080/calls/${m.entry.path}.wav`);
  }
};
```

## Serving your interface

Let the recorder serve your interface. Each interface gets **a port of its
own** (8081, 8082, … beside the built-in interface's 8080), and the
recorder's API is served on that port too: `/api/client.js`, `/api/ws` and
`/calls/` are right there, on the page's own origin, so it needs no access
settings. And because each port is a site of its own to the browser, your
page can't read the built-in interface's storage, or another interface's,
nor they yours.

An interface is **a folder with an `index.html`**, plus whatever else it uses:
scripts, styles, images, more pages. There's no server side: everything runs
in the browser. There's no build step unless you want one; a bundler's output
folder (`dist/`) works too.

**On the Interfaces page** (or `trunk-pro interface add <folder>`): **Add a
folder…** serves it on the next free port, at once. In the config file:

```json
"server": {
  "port": 8080,
  "interfaces": {
    "wall": { "port": 8081, "enabled": true, "access": "listen", "path": "/home/me/wall-display" }
  }
}
```

- The key is its id: lowercase letters, digits and `-`. `path` is the
  folder: absolute, `~/…`, or relative to the config file's folder. Without
  `path`, it's the copy installed from the interface store.
- `access` is what the page may do with the API: `"listen"` (the default)
  or `"control"`. See [Access](#access).
- Changes to ports, `enabled` and `access` take effect at once. No restart.

**For one run:** `trunk-pro --ui <folder>` serves the folder on the next free
port with control access, and opens it. Nothing is saved, which makes it
handy while developing.

**An `interface.json`** beside `index.html` names it and says what it needs.
It's optional for a folder of your own, and required to publish it in the
interface store ([docs/interfaces/publishing.md](../interfaces/publishing.md)):

```json
{ "id": "wall", "name": "Wall display", "version": "1.0.0", "description": "Every system's calls, big.", "api": 1, "access": "listen" }
```

**How files are served:**
- `/` is the folder's `index.html`, and `/app.js` its `app.js`. A subfolder
  serves its own `index.html`.
- A path with no extension that isn't a file (`/settings`) gets the
  folder's `index.html`. Pages that route themselves this way work; hash
  routes (`#/settings`) work anywhere.
- `/api/…` and `/calls/…` are the recorder's: don't put files there.
- Files are read on every request, with `Cache-Control: no-cache`: edit, then
  reload.
- Nothing outside the folder is served, symbolic links included, and no
  hidden files (names starting with `.`).

### Access

What a page may do depends on the port it came from:

| | `listen` (an interface's default) | `control` (the built-in interface) |
|---|---|---|
| Status, calls on the air, recorded calls, live audio, the dashboard's figures and queries | ✓ | ✓ |
| The config | without plugin settings and key sources | all of it |
| `setConfig`, `start`, `stop`, `quit`, plugins, interfaces, keys, the survey, files on the recorder's computer | refused with an `error` | ✓ |

A listen-only connection may send `subscribe`, `statsQuery`, `radioQuery`,
`olderCalls` and `listen`; anything else is answered with an `error`. Its
`hello` has no `configPath`, and it isn't sent `devices`, `plugins`,
`pluginRuntime`, `interfaces`, `store`, `installProgress` or `keys`. An interface that needs more says
`"access": "control"` in its `interface.json`; the store shows that before
it's installed, and the user can change it either way at any time.

## client.js

`/api/client.js` (in the repository: `docs/api/client.js`) is an ES module
with no dependencies. It works in browsers and in Node 22+. You don't have to
use it, since the protocol is simple, but it takes care of the fiddly parts:
reconnecting, re-sending `listen` after a reconnect, keeping state, safe
config edits, and gapless audio.

```js
import { TrunkClient, LivePlayer, decodeAudioFrame, parseTalkgroups, systemName, duration, mhz, escapeHtml } from "/api/client.js";
```

From a page served elsewhere, import it by its full URL and say which
recorder to use. The page's origin must be allowed; see
[Access from elsewhere](#access-from-elsewhere).

```js
import { TrunkClient } from "http://radio.local:8080/api/client.js";
const rec = new TrunkClient({ server: "radio.local:8080" });
```

### `new TrunkClient(opts?)`

| Option | Default | |
|---|---|---|
| `server` | the page's host; Node: `localhost:8080` | `host:port` of the recorder |
| `secure` | when the page is https | use `wss://` and `https://` |
| `historyLimit` | 500 | recorded calls kept in `history` |

**State.** Read these properties; don't change them. They are current
whenever `change` fires.

| Property | |
|---|---|
| `connected` | `true` while connected (from `hello` on) |
| `version` | the recorder's version |
| `capabilities` | what the recorder can do beyond recording (`hello.capabilities`): `service`, `drivers`, `plugins`, `interfaces`, `host`, `keys`, `calls`, `history` |
| `config` | the config (`Config`) |
| `phase` | `{ phase: "idle" \| "starting" \| "running" \| "stopping", error, ended }` |
| `status` | `EngineStatus` while running: `nowS`, totals, `systems[]` |
| `sources` | `SourceStatus[]`: each radio |
| `calls` | `CallView[]`: on the air now (empty when not running) |
| `history` | `CallEntry[]`: recorded calls, newest first |
| `configVersion` | the config's version, which `setConfig` sends |
| `talkgroupFiles` | each system's talkgroup file as CSV text: short name → CSV |
| `log` | the latest 500 `LogLine`s |
| `aliases` | talker aliases heard on the air: short name → `{ unit: alias }` (those saved, from `units`, then each `unitAlias`) |
| `error` | the latest `error` message's text, or why a `setConfig` was refused |
| `exited` | the recorder said it was exiting (`quit`); it keeps trying to reconnect, and this clears when it's back |

**Events.** `rec.on(type, fn)` returns a function that removes the handler.

| `type` | `fn` gets | When |
|---|---|---|
| `"change"` | the client | after any message: redraw. While a connection is being greeted, only once, at `hello` |
| any message type, e.g. `"concluded"` | the message | that message arrived (after the state was updated) |
| `"message"` | the message | every message |
| `"audio"` | `{ system, callId, talkgroup, samples }` | a live-audio chunk (`Float32Array`, 8000 per second) |
| `"connection"` | `true` / `false` | connected (at `hello`) / disconnected |

**Methods.**

| | |
|---|---|
| `send(msg)` | send a command (queued while disconnected) |
| `start()`, `stop()` | start / stop recording |
| `listen()`, `listen({ talkgroup })`, `listen({ system, talkgroup })`, `listen(false)` | live audio: every call, one talkgroup's, one system's (by short name) talkgroup's, none. Kept across reconnects. |
| `subscribe(topics)` | the costly messages to receive: `["log"]` fills `log`; `"spectrum:0"` a waterfall's `spectrum`. Kept across reconnects. |
| `setConfig(edit)` | `edit(copy)` changes a copy of the latest config, and the whole config is sent with `configVersion`. If the config changed elsewhere meanwhile it's refused, and `error` says so |
| `setTalkgroups(shortName, csv)` | replace a system's talkgroup file (`""` for none) |
| `setTalkgroup(shortName, talkgroup, edit)` | change one talkgroup's cells (`{ alphaTag, description, tag, category, ignore }`, any of them) |
| `callUrl(entry, "wav" \| "json" \| "m4a")` | a recorded call's file URL |
| `talkgroups(shortName)` | the system's talkgroup file, parsed from `talkgroupFiles`: `Map` talkgroup → `{ talkgroup, alphaTag, description, tag, group, mode }`; a conventional system's channels (`alphaTag` the channel's name) |
| `unitName(shortName, unit)` | a radio's name, as the system's `unitNames.mode` says: its unit names (`unitNames.csv`, regular-expression rows too) and the alias heard; else `""` |
| `close()` | disconnect for good |

### `new LivePlayer(opts?)`

Plays the chunks from `on("audio")`: `player.play(chunk)`. Call
`player.resume()` from a click or key press first; browsers won't start
sound otherwise.

| Option | Default | |
|---|---|---|
| `followCall` | `true` | one call at a time: the first heard, until quiet for `quietS`; overlapping calls are skipped |
| `quietS` | 1.5 | silence that ends a call, s |
| `gain` | 2 | volume (the vocoders' output is quiet) |
| `onNowPlaying` | — | `(call \| null) => …` when the call playing changes |

`player.nowPlaying` is `{ callId, system, talkgroup }` or `null`;
`player.stop()` releases the audio output.

**Helpers:** `duration(s)` gives `"1:05"`, `mhz(hz)` gives
`"155.2000 MHz"`, `escapeHtml(s)`, and `parseTalkgroups(csv)` reads a
talkgroup CSV. `systemName(status, system)` turns the `system`
number a call or audio chunk carries into its short name
(`status.systemNames`).

## Examples

In [`examples/`](examples/). A recorder serves them at
`/api/examples/<file>`, so they run as they are.

| | |
|---|---|
| [`scanner.html`](examples/scanner.html) | Everything in one file **without** client.js: systems, calls on the air, recorded calls, start / stop, live audio. Read it to see the protocol at work. |
| [`wall.html`](examples/wall.html) | A wall display: calls on the air in large type, emergencies first and red, recent calls, live audio. `?talkgroups=101,202`, `?system=…` |
| [`talkgroup.html`](examples/talkgroup.html) | One talkgroup: pick it, see it on the air, listen live, play its recordings, play each new recording as it arrives. |
| [`call-log.mjs`](examples/call-log.mjs) | A program (Node): append each recorded call to a CSV file, and POST it to a webhook. |

To make one of the pages your own, copy it into a folder as `index.html`
and add the folder as an interface.

## Access from elsewhere

The API has **no authentication**. Anyone who can reach the port can change
the config, start and stop recording, and list folders on the recorder's
computer. Two things keep that safe:

- **The recorder only listens on `127.0.0.1`** unless it's told otherwise
  (`--bind 0.0.0.0` or `server.bind` in the config). Only do that on a network
  you trust, or put it behind a reverse proxy that adds authentication.
- **Web pages from other sites are refused.** Every web page a browser shows
  could otherwise reach `ws://localhost:8080`. A browser sends the page's
  origin with each request. Each port accepts its own pages and anything that
  isn't a browser (no `Origin` header: scripts, Node, curl). It answers any
  other origin with **403** and logs `Refused a page from …` once. So an
  interface on port 8081 uses the API on 8081, with its own access: it can't
  borrow the built-in interface's by connecting to 8080.

To use the API from a page the recorder **doesn't** serve (a dev server, a
page on another machine, a file), add the page's origin to `allowedOrigins`
in the `server` section of the config, or on the Interfaces page under
**Pages served elsewhere**. Such a page gets control access on the port it
uses:

```json
"server": {
  "allowedOrigins": ["http://localhost:5173", "null"]
}
```

- An origin is `scheme://host[:port]`, exactly as the browser sends it, e.g.
  `http://192.168.1.50:3000`. It has no path.
- `"null"` is what a page opened from a file (`file://…`) sends. Note that
  browsers don't load ES modules into such pages, so client.js needs a served
  page.
- `"*"` allows every page. Only use it on a recorder nobody else can reach.
- Allowed origins also get CORS headers, so a page can `fetch()` from
  `/calls/` and `/api/`, and import `/api/client.js`.
- Changing the config through the API (`setConfig`) takes effect at once;
  editing the file needs a restart.

On a recorder bound to localhost, the `Host` header must also be a localhost
name. This blocks DNS rebinding, where another site points its name at
127.0.0.1. A reverse proxy that passes its own `Host` therefore needs its
origin listed (e.g. `"https://radio.example.com"`).

## How a connection goes

1. **Connect** to `ws://<host>:<port>/api/ws`. Set `binaryType = "arraybuffer"`
   if you'll want live audio.
2. **The greeting.** The recorder sends how things are now as ordinary
   messages, the same ones it sends later when they change: `state`,
   `config`, `talkgroups` (one for each system), `units` (each system with
   some), `heard`, `history` (the last 300 recorded calls), `radios`,
   `survey`, `monitorEvent` (each recent event), `host`; with control access
   also `devices`, `keys`, `plugins` and `interfaces`. **`hello`** comes
   last. It means you're caught up: draw the first screen then, and don't
   treat something missing as missing before it (a system with no
   `talkgroups` message has no talkgroup file).
3. Then **messages as things happen**, to every connected client:
   - `state` when recording starts or stops
   - `status` about twice a second while recording: systems, sources and the
     calls on the air
   - `stats` once a second while recording: every measured series' value
     (see [Statistics and history](#statistics-and-history))
   - `host` every 2 s: the computer and the plugins (desktop app)
   - `concluded` for each recorded call
   - `monitorEvent` when something notable happens
   - `config` when anyone changes the config (with its `version`)
   - …the full list is [below](#messages-from-the-recorder).
4. **Subscribe** to what costs the recorder something to make, while you
   show it: `{"type": "subscribe", "topics": ["spectrum:0", "log"]}`. A
   connection gets none of these until it asks; each `subscribe` replaces the
   last. Topics: `spectrum:<source>` (the waterfall, ~7/s), `log` (every
   control channel message as a line), `rf:<source>` (`rfDetail`, 1/s),
   `decode:<shortName>` (`decodeDetail`, 1/s), `platform` (per-core and
   per-interface figures in `host`). With nobody subscribed the recorder
   doesn't compute them.
5. **Send commands** as JSON text whenever you like. Answers to your own
   commands (`error`, `notice`, `dir`, `trConfig`, `devices`, `radios`,
   `store`, `configSaved`, `subscribed`, `statsResult`, `radioResult`, `olderCalls`,
   `licenseResult`, `callSignResult`, `pluginActionResult`, and `plugins` when you sent
   `plugins`) come to your connection only. A command the recorder can't
   read (a field missing or of the wrong kind, an unknown `type`) is answered with an `error`. Everything else goes to every client: after
   `addPlugin`, `removePlugin` or a plugin's `install` everyone gets `plugins`,
   and after `setConfig` or `channelFile` everyone gets `config`; every
   `setConfig` is answered to its sender with `configSaved`.
6. **Reconnect** when the socket closes: the recorder may have restarted. A
   new greeting brings you up to date. Per-connection settings (live audio,
   subscriptions) must be sent again after it.
7. **`quit`** means the recorder is exiting (Quit, Ctrl-C, a service stop or
   restart). The built-in interface stops there. A display that should come
   back after a restart keeps trying, more slowly. client.js retries every
   5 s and sets `exited`.

Unknown message types should be ignored: new ones may be added.

## Concepts

**Phase.** The recorder is `idle`, `starting`, `running` or `stopping`.
The `state` message carries it, with `error` (why it stopped
or couldn't start) and `ended` (a capture file played to its end).

**Systems.** `config.systems` are trunked systems: P25, SmartNet, DMR,
NXDN and EDACS. Each has its own `shortName`, which is also the folder its calls are
saved in, and its `sites`, each a control channel it is recorded from. A call
heard on several sites of a system is one call, saved once; a call's `site`
(and `alsoOn`, the other sites carrying it) are site names. While running,
`status.status.systems[i].index` is the number that calls and live audio
carry as `system`, and `systems[i].sites[]` has each site's control channel,
identity and decode counts. A site's identity (`nac`, `wacn`, `sysId`, `rfss`, `site`) leaves out
what hasn't been heard yet, so test a field with `!= null`. It is the system's position among the systems
being **recorded** (enabled, with a site that has a control channel), so
match on `shortName` rather than on positions in the config.

**Conventional systems.** `config.conventional[k]` are groups of fixed
frequencies. Their calls are numbered after the trunked systems' (and
`status.systemNames` names every number); while running they're in
`status.systems` too, after the trunked ones, with `sites: []` and
`conventionalOpen` (channels open now). Each channel's calls are filed under
a talkgroup: the channel's `talkgroup` (TG Number); without one, a digital
channel's (P25, DMR, NXDN) the talkgroup the air names, an analog channel's
its frequency in kHz (a later row on one frequency adds a digit: 1543251).

**Calls and transmissions.** A call is usually a talkgroup's conversation, its
transmissions until it has been quiet for the call timeout. A system with
`recording.conversationMode` false (its own `recording` rules) makes each
transmission a call of its own. On a system with several sites every copy is
recorded and the best saved; a copy is dropped only when the saved one holds
all of it, so a few may be saved twice. Copies of one transmission saved share
their `CallRecord.transmission_id`.

**Calls on the air vs. recorded calls.**
- `status.calls` (`CallView[]`) are the calls happening right now. That
  includes ones being followed but not recorded: `state: "monitoring"` with a
  `reason` such as `"encrypted"` or `"unknown_tg"`.
- A recorded call arrives once, finished, as `concluded`. Its `entry` is a
  `CallEntry`: `path` (where its files are) and `record`, Trunk Recorder's
  call JSON (`CallRecord`). The same entries make up the `history` message, newest
  first.

**Talkgroups** are numbers. Their names (`alphaTag` / `talkgroup_tag`) come
from each system's talkgroup file (the `talkgroups` message), and are
`""` when the talkgroup isn't in it. **Units** (radios) are numbers too:
`CallView.sources`, `CallRecord.srcList`. Their names come from the
user's unit names (`unitNames` of each system in the config), the talker
aliases heard on the air (the `units` messages, then the `unitAlias` messages),
and `srcList[].tag`.

**Times.** Engine times (`nowS`, `startS`, `timeS`) are seconds since
recording started. Times in call records are Unix seconds / milliseconds.
Frequencies are Hz everywhere.

## Messages from the recorder

Every message is a JSON object with a `type`. Exact fields are in
`FromRecorder` in protocol.ts.

| `type` | When | What's in it |
|---|---|---|
| `hello` | Last of a connection's greeting: you're caught up | `version`, `platform`, `api`, `configPath` (control access only), `surveyBands`, `capabilities`: what it can do beyond recording, of `service` (a program of its own: quit, files by path, the log, M4A encoding), `drivers` (USRP, Airspy, SoapySDR, finding radios), `plugins`, `interfaces`, `host`, `keys`, `calls` (the call store, for `callsQuery`) and `history` (the dashboard's history further back than a week, for `statsQuery`'s `30d`, `90d` and `1y`; the desktop app has all of them, `calls` and `history` while its call store is open, and the browser build none: show only what works, and a command for one it lacks gets an `error`). The rest of the greeting is the messages below, sent first |
| `history` | On connecting | `entries`: the latest calls (≤ 300 `CallEntry`), newest first. Then `concluded` for each new one |
| `units` | On connecting, for each system with some | `system` (short name), `csv`: the talker aliases saved, as CSV: `unit,alias,source,time,wacn,sys,tg`, the newest line per unit wins. Then `unitAlias` for each new one |
| `talkgroups` | On connecting, for each system; whenever one is replaced | `system` (short name), `csv`: the system's talkgroup file (Trunk Recorder's CSV format; `""`: none) |
| `talkgroupEdited` | One talkgroup of a system's file was edited (`setTalkgroup`) | `system`, `talkgroup`, `edit`: the cells it set. Make the same edit to the file you keep: only that row changes, a column or row is added when missing (`client.js` does this) |
| `plugins` | On connecting (control access); after plugin changes; answers `plugins` | `plugins` (`PluginInfo[]`), `encoderFound` |
| `state` | Recording starts / stops | `phase`, `error`, `ended` |
| `status` | ~2/s while running | `status` (`EngineStatus`: totals, `systemNames`: every system's short name by its number, `systems[]` (trunked, then conventional) with `share` (a system shared with the directory: `{ state: "needsCallSign" | "waiting" | "shared" | "error", lastShared, error }`, else null), and each site's control channel, decode counts `good`/`bad`, site identity, `callsigns` (the FCC call signs a Motorola control channel sends, each with its `freqHz`); neighbours, patches and the like are in `decodeDetail`), `sources[]` (each radio: its rate as set and `rateMeasured`, drops, errors and the last (`lastError`, `lastErrorS`), frequency error measured `errorPpm` and the correction `tunePpm`), `load` (share of real time the decoder is busy), `calls` (`CallView[]`, on the air now) |
| `spectrum` | Topic `spectrum:<source>`: ~7/s while running | `source`, `centerHz`, `rateHz`, `bins`: 512 levels in dBFS, lowest frequency first, spanning `centerHz ± rateHz/2` |
| `stats` | 1/s while running | `t` (Unix s), `values`: series name → value now ([Statistics and history](#statistics-and-history)) |
| `host` | Every 2 s (desktop app) | `t`, `values` (the computer's and plugins' series), `platform` (OS, cores, container and its limits, `disks[]` (`{ name, path, mount, totalBytes, freeBytes }`; the RAM spool's as `spool`, with `kind` (`"RAM disk"`, `"tmpfs"` (/dev/shm), `"service"` (systemd's runtime folder) or `"folder"`), calls `overflowed` to disk, times it was `removed` while running, and `missing`: gone and not made again), `probes[]` (connectivity checks), `clock` (the clock's health: the operating system's sync and time service, a measured offset from `server`, the recording's drift, the radio systems' clocks, and `problem`: the recorder's verdict), `spotlight` (macOS: whether Spotlight indexes the recordings folder), `unavailable`: what this platform can't tell and why) |
| `rfDetail` | Topic `rf:<source>`: 1/s | `source`, `profile` (the noise floor in 64 slices across the band, dBFS per FFT bin), `channels` (each channel on it: power, floor, SNR, carrier offset, the receiver's eye opening or CQPSK phase error) |
| `decodeDetail` | Topic `decode:<shortName>`: 1/s | `system`, `channels` (the system's channels as received now, each with its `site`), `sites` (each running site's `index`, `name`, and what its control channel announces: `adjacent` sites, `dmr`, `nxdn`), `patches` (the patches standing now, each once: `supergroup` and `members`) |
| `monitorEvent` | Something notable happened | `t`, `level` (`info`, `ok`, `warn`, `bad`), `kind` and its fields: control channels (`controlLost`, `controlRegained`; `system`, `site`), talkgroups and radios (`tgFirstSeen`, `tgActive`, `unitFirstSeen`, `unitActive`, `unitAffiliated`, `callVolume`), tones (`tonePattern`: `text`, `signals`), radios (`sourceDrops`, `sourceClipping`, `sourceRecovered`), plugins (`pluginHealth`), the computer (`linkDown`, `linkUp`, `diskLow`), the RAM spool (`spoolLow` with `critical`, `spoolFull` with `calls`, `spoolRecovered`, `spoolRemoved` with `spool` (its kind) and `remade`), the clock (`clockOff` with `why` and `bad`, `clockOk`, `clockStep` with `byMs`). The event feed's kinds are listed in protocol.ts (`MonitorEvent`) |
| `subscribed` | Answers `subscribe` | `topics` |
| `olderCalls` | Answers `olderCalls` | `before` (as asked), `entries` (`CallEntry[]`, newest first), `more` (there are older ones still) |
| `callsResult` | Answers `callsQuery` | `id` (as asked), `entries` (`CallEntry[]`, newest first), `more` (there are older ones still), `before` (where the next page starts: send it as `before`; with no entries, the `before` asked, or 0 when none was), `error?` (the call store is off or failed: the page is the recent calls alone) |
| `keys` | On connecting, and after an unlock, a lock or a config change (control access) | `sources[]`: `{ name, systems, unlocked }`, each key container that waits for a password |
| `statsResult` | Answers `statsQuery` | `id`, `from`, `to`, `loading` (the history files are still being read), `series`: name → `{ t0, stepS, v, lo, hi, n }` |
| `radioResult` | Answers `radioQuery` | `id`, `what`, and by `what`: `rows` (talkgroups / radios / frequencies / tone patterns heard), `outOfBand` (freqs), `row` and its detail, `systems` (summary), `histogram` |
| `pluginActionResult` | Answers `pluginAction`, to its sender | `id`, `ok`, `message`: what the plugin said of it (or why there's no answer: it isn't running, or didn't answer within 15 s) |
| `concluded` | A call was recorded | `entry`: `{ path, record, audio, json }`; `audio` / `json`: whether they stay in the recordings folder once the plugins are done (`audio`: some audio, .wav or .m4a, stays: **Keep .wav** / **Keep .m4a**; `json`: **Keep call JSON**; false: deleted then) |
| `callFiles` | A call's upload plugins are done, and it kept other than its entry said (an upload failed and its files stay) | `path`, `audio`, `json` |
| `log` | Topic `log` (warnings and errors always) | `lines[]`: `{ timeS, kind, text, system?, site?, time?, level? }` — `kind` e.g. `"error"`, `"control"`, `"alias"`, `"plugin"`, `"info"`, or a control-message kind; the decoder's lines have `timeS` (seconds into the recording), the recorder's own `time` (Unix seconds) and `level` |
| `unitAlias` | A talker alias was heard | `system` (short name), `unit`, `alias` |
| `heard` | On connecting; conventional codes heard changed | `heard`: frequency (Hz, as a string) → `HeardCode[]` |
| `config` | On connecting; the config changed (anyone's `setConfig`, a plugin added…) | `config`, `version` (what an edit is sent with) |
| `configSaved` | Answers `setConfig`, to its sender | `version` (the config's now), `error` when the edit wasn't taken (the config changed elsewhere meanwhile, or it's bad) or couldn't be saved |
| `devices` | On connecting (control access); answers `devices` | `devices`: RTL-SDR dongles plugged in (`busy` when another program has one) |
| `radios` | On connecting; answers `findRadios` | `radios`: the optional USRP / Airspy / SoapySDR drivers and their devices |
| `dir` | Answers `listDir` | a folder: `path`, `parent`, `dirs`, `files` (.json), `home`, `sep`, `error` |
| `trConfig` | Answers `readTrConfig` | a Trunk Recorder config.json's `text`, the `files` it names, or `error` |
| `survey` | The survey's progress | `stage` (`idle` / `scanning` / `monitoring` / `done`), candidates, the channel monitored, the suggested system |
| `licenseResult` | Answers `licenseQuery`, to its sender | `id`, `results[]` of `{ freqHz, historical, licenses[] }` (each license: `callSign`, `licensee`, `serviceCode`, `serviceDesc`, `freqHz`, `emission`, `distanceM`, `bearingDeg`, `bearingCardinal`, `city`, `state`, `status`, `locationKind`; `historical`: none active, so lapsed ones), or `error` |
| `surveySpectrum` | While surveying | like `spectrum` |
| `sourceProfile` | Answers `profileSource` | `source`, then `error`, or `centerHz`, `rateHz`, `spectrum` / `floor` (1024 points, dB, −rate/2 … +rate/2), `rows` (a waterfall), `referenceDb`, `lowHz` / `highHz` (how far in the floor is 3 dB down), `lowDropDb` / `highDropDb`, `suggestedGuardHz` |
| `pluginRuntime` | A plugin's totals changed (control access) | `id`, `runtime` (state, counts, recent log; `metrics` — queue, upload time, endpoints — when the plugin reports them; restarts, events dropped, uptime; results per minute for the last hour) |
| `interfaces` | On connecting (control access);  after interface changes; answers `interfaces` | `interfaces` (`InterfaceInfo[]`: each one's port, access, whether it's served and why not), `api` |
| `store` | Answers `store` | `kind` (`plugin`: the plugin registry; `interface`: the interface store), `source`, `fetched`, `problem`, `listings` (a plugin store also says this computer's `target`) |
| `installProgress` | As an install goes (control access) | `kind`, `key` (what was asked for), `id`, `stage` (`finding` … `done` / `failed`), `message` |
| `error` | A command failed | `message`, for the user |
| `notice` | A command worked and has something to say | `message` |
| `quit` | The recorder is exiting (it may be restarted) | — |

## Commands to the recorder

Send JSON text. Exact fields are in `ToRecorder` in protocol.ts. A command that can't be read
(unknown `type`, a field missing or of the wrong kind) is answered with an `error` saying what was
wrong; one a listen-only connection may not send, or the browser build can't do, with an `error`
too.

| `type` | Fields | Does |
|---|---|---|
| `start` | — | Start recording with the saved config. `state` follows; `error` if it can't. |
| `stop` | — | Stop recording. Calls in progress are saved. |
| `listen` | `on`, `system` (a short name, or null), `talkgroup` (number or null) | Live audio on this connection: every call (both null), one system's, one talkgroup's, or both. `on: false` stops it. |
| `setConfig` | `config`, `version` | Replace the whole config, edited from `version` (the last `config` message's): saved, and sent to everyone as `config`. Refused if the config has changed since (another browser saved). Always answered with `configSaved`. Takes effect at the next `start` (plugin settings at once). |
| `setTalkgroups` | `system` (short name), `csv` | Replace a system's talkgroup file (`""` removes it). Everyone gets it as `talkgroups`; a recording takes it at once. |
| `setTalkgroup` | `system`, `talkgroup`, `edit`: any of `alphaTag`, `description`, `tag`, `category` (text), `ignore` (true/false) | Change one talkgroup's row: only the cells given (a row added when the file lacks it and something is set). Everyone gets it as `talkgroupEdited`; a recording takes it at once. For an Ignore button or a name edit, rather than sending the whole file. |
| `channelFile` | `index`, `path` | Link conventional system `index`'s channels to a CSV, reload it (same path), or unlink (`""`). |
| `devices` | — | List RTL-SDR dongles (→ `devices`). |
| `findRadios` | — | Look for USRP / Airspy / SoapySDR devices (→ `radios`; takes a few seconds). |
| `listDir` | `path` (`""` = home) | List a folder on the recorder's computer (→ `dir`). |
| `readTrConfig` | `path` | Read a Trunk Recorder config.json and its files, to import (→ `trConfig`). |
| `surveyStart` | `source`, `bands` (ids from `hello.surveyBands`), `findGain` | Scan for systems with radio `source` (not while recording). |
| `surveyListen` | `freqHz` | Monitor this signal. |
| `surveyRescan` / `surveyStop` | — | Scan again / end the survey. |
| `licenseQuery` | `id`, `lat`, `lon`, `radiusM`, `signals[]` of `{ freqHz, widthHz? }` | Who is licensed on these frequencies within `radiusM` (at most 150 km) of the location (→ `licenseResult`). The recorder sends the frequencies and the location to trunkrecorder.pro (`server.trpApi`); only when the user asks. |
| `callSignQuery` | `id`, `callSigns[]` | Who holds these call signs (heard on the air), from trunkrecorder.pro, no location (→ `callSignResult`: `id`, `results[]` of `{ callSign, licensee, serviceCode, serviceDesc, status, city, state }` for each active one, or `error`). |
| `profileSource` | `source`, `centerHz` | Look at radio `source`'s band for a couple of seconds, tuned to `centerHz` (a capture keeps its own), and measure how it rolls off at the edges (→ `sourceProfile`; not while recording). |
| `plugins` | — | List plugins again (→ `plugins`). |
| `addPlugin` | `path` | Add a plugin executable of your own. |
| `removePlugin` | `id` | Remove a plugin. |
| `interfaces` | — | List the interfaces again (→ `interfaces`). |
| `store` | `kind` (`plugin` or `interface`), `refresh?` | A store's list (→ `store`). |
| `install` | `kind`, then `id`, or `repository` and `tag?` | Install or update a plugin or an interface from its store or a GitHub release (→ `installProgress` as it goes). A new interface is served at once on the next free port. |
| `addInterface` | `path` | Serve a folder on the recorder's computer as an interface. |
| `removeInterface` | `id` | Stop serving it and forget it; an installed copy is deleted. |
| `subscribe` | `topics` | What this connection watches (replaces the last): `spectrum:<source>`, `log`, `rf:<source>`, `decode:<shortName>`, `platform` (→ `subscribed`). |
| `statsQuery` | `id`, `series` (names, or prefixes ending in `*`), `range` (`10m` / `1h` / `6h` / `24h` / `7d` / `30d` / `90d` / `1y`) or `from` / `to` (Unix s), `points?` | The history of series, averaged down to about `points` steps (→ `statsResult`). Up to a day (a week without a store) is answered from memory by the minute; further back (`7d` with a store, `30d`, `90d`, `1y`, or an older `from`) is the store's hours, needs `history` in `hello.capabilities`, and its steps are whole hours or days, as far back as `storage.statsDays` keeps. |
| `radioQuery` | `id`, `what`, `system?`, `key?`, `hours?`, `limit?` | The radio registry: `summary`, `talkgroups`, `units`, `tg` (`key`: the talkgroup), `unit` (`key`: the radio), `freqs`, `lengths`, `tones` (→ `radioResult`). |
| `pluginAction` | `id`, `plugin`, `action`, `value` | Press one of a plugin's buttons (its settings schema's `x-actions`) on `value`, the settings object it's on as edited (→ `pluginActionResult`, once the plugin answers). Control access; plugins run only while recording. |
| `olderCalls` | `before?` (Unix ms), `limit?` (≤ 500, default 100) | `callsQuery` with only `before` and `limit`, answered as `olderCalls` (kept for interfaces that use it; new ones send `callsQuery`). Listen-only connections may send it. |
| `callsQuery` | `id`, `system?` (short name), `talkgroup?`, `unit?` (a radio), `from?` / `to?` (start time, Unix ms: `from` inclusive, `to` exclusive), `before?` (Unix ms: the last page's `before`), `limit?` (1 to 500, default 100) | Recorded calls, newest first (→ `callsResult`). The desktop app answers from its call store with the latest calls included; the browser build from its own list. Listen-only connections may send it. |
| `unlockKeys` | `source`, `password` | Open a key container that has no password file; the password is kept in memory only. `keys` follows. |
| `lockKeys` | `source` | Forget a container's keys: its systems stop decrypting at once. |
| `quit` | — | Stop recording and exit the recorder. |

A listening-only interface needs nothing but `listen`, and perhaps
`start` / `stop`.

## Live audio

After `{"type": "listen", "on": true, "system": null, "talkgroup": null}`,
the recorder sends each call's audio as it's decoded, as **binary** WebSocket
messages:

| Bytes | Type | |
|---|---|---|
| 0 | u8 | version: `2` |
| 1–2 | u16 LE | `system`: the system's number this run (as in `CallView.system`; `status.systems[].index`) — map it to its short name |
| 3–6 | u32 LE | call id (`CallView.id`) |
| 7–10 | u32 LE | talkgroup |
| 11– | i16 LE … | samples: mono, 8000 per second |

```js
function decode(buf) {
  const v = new DataView(buf);
  if (v.getUint8(0) !== 2) return null;
  const samples = new Float32Array((buf.byteLength - 11) >> 1);
  for (let i = 0; i < samples.length; i++) samples[i] = v.getInt16(11 + 2 * i, true) / 32768;
  return { system: v.getUint16(1, true), callId: v.getUint32(3, true), talkgroup: v.getUint32(7, true), samples };
}
```

Audio arrives in bursts (digital voice is decoded a fraction of a second at a
time), and calls on different talkgroups can interleave. To play it, queue
each chunk right after the previous one on an `AudioContext`, a little ahead
of `currentTime`. After a gap, start again a little ahead. To follow one call
at a time, keep playing the `callId` you started with until it goes quiet.
client.js's `LivePlayer` does all this; `examples/scanner.html` shows the
simplest version by hand.

Browsers only start audio after a click or key press: create or `resume()`
the `AudioContext` in a click handler. Listening is per connection and off
after a reconnect, so send `listen` again after `hello`.

## Recorded calls

The call store is a database of every recorded call, kept beside the config (`trunk-pro.sqlite`). `callsQuery` searches it by system, talkgroup, radio and time, as far back as it keeps calls (`storage.retentionDays`); `history`, `olderCalls`, `GET /api/v1/calls` and plugins' `calls.query` are the same query. Every answer includes the latest calls, the store's not-yet-committed ones too. `hello.capabilities` has `calls` while the store is open; when it's off, the same messages answer from the recent calls alone (`callsResult.error` says so).

Each recorded call's files are under the recordings folder at
`CallEntry.path`:

- `/calls/<path>.wav`: the audio, 8 kHz mono
- `/calls/<path>.json`: the same `CallRecord` that came in `concluded`
- `/calls/<path>.m4a`: when an .m4a was kept (`recording.archiveM4a`). The server falls back
  between the `.wav` and the `.m4a` for whichever exists

URL-encode each segment of the path:
`path.split("/").map(encodeURIComponent).join("/")`. An `<audio>` element can
play the `.wav` directly. The files can be deleted once the plugins are done
with them (`recording.archiveWav` and `archiveM4a` off), so older calls in the history
may have no files.

`CallRecord` is Trunk Recorder's call JSON, so tools written for Trunk
Recorder's files read it too. The fields you'll want most: `talkgroup`,
`talkgroup_tag`, `short_name`, `start_time` (Unix s), `call_length` (s),
`freq`, `emergency`, `encrypted`, `srcList` (the radios that talked, with
names in `tag`), and the reception fields `signal` / `noise` / `snr` (dB).
Beyond Trunk Recorder's: `transmission_id` (the same on every copy of one
transmission saved, [above](#concepts)), `site`, `system_type` and
`voice_mode`, `clean_voice_pct`, `nac` (hex), `color_code` / `ran`,
`talkgroup_ota` (a conventional call's talkgroup as the air named it),
`decrypted` with `algid` / `key_id`, `clock_off`, and the call's in-band
`tones`, the `tone_patterns` they make up (each with `text` in words) and the
system's `signals` it carried. docs/guides/recordings.md lists every field.

### The call store over HTTP

The same store answers plain HTTP `GET`s, for a plugin or an interface on
another machine that has no WebSocket to keep. Every door that serves the API
serves these (the built-in interface and each interface's port), to a
connection with listen or control access, as `callsQuery` is; a door that is
off answers 404. All answers are JSON. When the call store could not be
opened, each answers 503 `{"error": "the call store is off"}`. (Plugins ask
over their own channel instead: `calls.query`, in the plugin SDK.)

| Route | Answers |
|---|---|
| `GET /api/v1/calls` | `{ entries, more, before }`, the page `callsResult` has: `CallEntry`s, newest first. Parameters, all optional: `system` (short name; its sites' old names too), `talkgroup`, `unit` (a radio), `from` (start time, Unix ms, inclusive), `to` (exclusive), `before` (Unix ms: pass the last answer's `before` for the next page), `limit` (1 to 500, default 100). A parameter given empty counts as not given; a bad number answers 400 `{"error": "<name> must be a number"}`. |
| `GET /api/v1/calls/<path>` | One `CallEntry`: `{ path, record, audio, json }`, for the `path` its entry has (slashes stay slashes); 404 `{"error": "no such call"}` when there is none. |
| `GET /api/v1/storage` | `{ backend, path, schemaVersion, calls, oldestMs, newestMs, retentionDays, commitSeconds }`: which backend (`sqlite`, `postgres`) and where the store is (a PostgreSQL database without its password), how many calls it holds and the start times of the oldest and newest committed (`null` when empty), and the settings `storage.retentionDays` and `storage.commitSeconds`. |

The routes are described in
[calls.openapi.yaml](calls.openapi.yaml) (OpenAPI 3.0).

## Recipes

Each recipe shows the raw protocol, then the client.js equivalent.

**Show what's on the air.** Redraw from each `status`: `m.calls` is the
complete list each time. A call with `state: "recording"` is being recorded.
With client.js: `rec.calls`, on `change`.

**How long a call has been going.** `status.nowS - call.startS` (both engine
seconds). With client.js: `rec.status.nowS - c.startS`.

**A call log.** Start from the `history` message and put each `concluded.entry` at
the front. It's the same shape either way. With client.js: `rec.history`.

**Listen to one talkgroup.** Send `{"type": "listen", "on": true, "system":
null, "talkgroup": 1234}`. Add `system` when talkgroup numbers repeat across
systems. With client.js: `rec.listen({ talkgroup: 1234 })`. For several
talkgroups, listen to everything and drop the chunks you don't want.

**Play new recordings one after another** (no live audio needed): on each
`concluded`, queue `/calls/<path>.wav`, and play the queue with one `<audio>`
element. `examples/talkgroup.html` does this.

**Change a setting.** Take the latest `config` message, change it, and send
the **whole** object back with `setConfig`, with that message's `version`.
Then wait for the `config` message that everyone gets, and the `configSaved`
that answers you: if its `error` is set the edit wasn't taken (the config
changed elsewhere meanwhile: take the new one and make the change again).
Never build a config from scratch: fields you leave out are reset to defaults.

```js
const next = structuredClone(config);
next.recording.minCallS = 2;
send({ type: "setConfig", config: next, version });
// client.js:
rec.setConfig((c) => { c.recording.minCallS = 2; });
```

**Start and stop.** Send `start` / `stop` and draw from `state`. Don't assume
the command worked: `error` says why it didn't.

**A talkgroup's name.** In a call: `alphaTag` (live) or `talkgroup_tag`
(recorded). Otherwise, each system's talkgroup file comes in a
`talkgroups` message (Trunk Recorder's CSV format); change one with
`setTalkgroups`. With
client.js: `rec.talkgroups(shortName).get(1234)?.alphaTag`.

**A radio's name.** In a recorded call: `srcList[].tag`. Live: the user's
names are in the config, `systems[i].unitNames.csv` (or
`conventional[k].unitNames.csv`): headerless `unit,name` lines, `#`
comments; a unit between slashes is a regular expression whose groups
(`$1` or `\1`) may appear in the name (`/^1(\d{3})$/,Engine $1`); the first
matching line wins. The aliases heard on the air are in
the `units` message for the system (Trunk Recorder's unitTagsOTA CSV:
`unit,alias,source,time,wacn,sys,tg`, the newest line per unit wins), then
in the `unitAlias` messages. `unitNames.mode` says which to use:
`"user"` (the default) the user's name first, then the alias; `"ota"` the
alias first; `"user_only"`; `"none"`.
With client.js: `rec.unitName(shortName, unit)`.

## Having an LLM build an interface

[`llms.txt`](llms.txt) (served at `/api/llms.txt`) is written for an LLM. It
covers what to build, how to deliver it, client.js, the data, the messages,
the rules that trip people up, and a template. Give it that, plus
`protocol.ts` for exact field names, and describe what you want:

> Read llms.txt and protocol.ts (attached). Build me an interface for
> Trunk Recorder Pro: a single index.html, no build step, using
> /api/client.js. It should show the calls on the air in large text for a
> wall display: the talkgroup's name, the system, and how long it's been
> going, with emergencies in red at the top. Under it, list the last 20
> recorded calls with a play button each. Add a toggle for live audio of
> one talkgroup, chosen from the talkgroups heard.

More ideas to ask for:
- A phone page: a big **Listen** button, the talkgroup now playing, skip and
  mute buttons for talkgroups.
- A daily summary: calls per talkgroup per hour, from `history`, as a
  heatmap.
- A monitor for one incident: a list of talkgroups to follow, live audio of
  those only, and a transcript-ready list of their calls with unit names.
- A status board for the recorder: each system's decode rate, each radio's
  dropped samples and frequency error, the recorder's load.

An agent that can run commands (Claude Code, for example) can do the whole
loop:

1. Make the folder.
2. Run `trunk-pro --ui <folder>`, or add the folder as an interface.
3. Open the address it prints (`http://localhost:8081/`), check it, and iterate.

The recorder serves the files as they are on disk, so a reload shows each
change.

## Statistics and history

While recording, the recorder measures itself once a second and keeps a
week of it, a minute at a time (`<data folder>/stats/*.jsonl`, gzipped after
the day). The live values come in `stats` (and `host`); the history answers
`statsQuery`. Each value has a name:

| Series | What |
|---|---|
| `src/<label>/noise`, `peak`, `clipPct` | A radio's noise floor (dBFS per FFT bin), its peaks (dBFS), and the share of samples at full scale |
| `src/<label>/samples`, `dropped`, `errors` | Samples a second, samples lost a second, driver errors |
| `src/<label>/ppm`, `tune` | Its frequency error as measured on the control channels, and the correction applied (ppm) |
| `sys/<short>/cc/good`, `bad` | Control messages decoded / lost a second |
| `sys/<short>/cc/locked`, `snr`, `signal`, `noise`, `offset`, `sep`, `phaseErr` | Decoding (1/0), the channel's SNR and levels, its carrier offset (Hz), the C4FM receiver's eye opening, the CQPSK receiver's phase error (RMS degrees: under 10 clean, ~25 noise) |
| `sys/<short>/cc/syncs`, `nidFails`, `flywheels`, `eqResets` | P25 framing: syncs found, headers failed, frames carried over a missed sync, equaliser resets (a second) |
| `sys/<short>/cc/airSym` | P25: symbols of control channel air decoded a second; over 4800, the coverage |
| `sys/<short>/calls`, `airMs`, `audioBytes`, `active`, `recording` | Calls ending a second, airtime (ms a second, counted while the calls are on the air), audio written, calls on the air and being recorded now |
| `sys/<short>/why/<reason>` | Calls starting a second by what happened to them: `recorded`, `monitored`, `ignored`, `encrypted`, `unknown_tg`, `no_source`, `no_recorder` |
| `sys/<short>/voice/frames`, `errors`, `bad` | Voice frames decoded, bit errors corrected, frames lost (a second) |
| `all/…`, `day/…`, `eng/…` | Every system together; this run's totals today; the decoder's load and recorders |
| `plat/cpu`, `proCpu`, `mem`, `memAvail`, `swap`, `memPsi`, `memLevel`, `temp`, `disk/<name>/free`, `freePct` (the RAM spool's as `disk/spool/…`), … | The computer (desktop app) |
| `net/up`, `rtt`, `dns`, `rx`, `tx`, `errors`, `probe/<target>` | Connectivity checks (share answering, connect time, each target's), name lookup time, traffic |
| `clock/synced`, `maxErrMs`, `offsetMs`, `delayMs`, `driftMs`, `radioMs` | The clock: synchronized (1/0) and the OS's error bound, the offset measured from the time server and its round trip, the recording's drift, the radio systems' clocks against it |
| `plg/<id>/ok`, `failed`, `queued`, `latency`, `state` | Each plugin |

Running totals arrive as rates (per second); everything else as it is.
`<label>` is the source's label with `/` made `_`. The `sys/<short>/cc/…`
series are a site's: `sys/<short>/cc/…` for a site named as its system (a
system of one site), `sys/<short>/site/<site>/cc/…` for the others. In a `statsResult`, `v`
is each step's average, `lo` / `hi` its lowest and highest minute, and `n`
the minutes measured in it — so a rate's amount over a step is `v × n × 60`.

The **radio registry** (`radioQuery`) is what the recorder has heard of each
system: talkgroups (first and last heard, calls and airtime by hour, whether
in the talkgroup file or ignored), radios (transmissions, affiliation, the
talkgroups they use, the radios heard with them), and each frequency's voice
quality and calls out of band (`freqs`: `rows`, the frequencies with a call
recorded; `outOfBand`, those granted calls no source covered). It is kept between runs (`<data folder>/radio/`).

## Compatibility

protocol.ts is the definition. The recorder's tests check what it actually
sends against the schema generated from protocol.ts: every message type in
both directions, and the shapes of the main messages (`hello`, `status`,
`concluded`, `config`, the survey's, the plugins'…). So this page and the
types stay true to the recorder you're running.

The protocol can grow: new message types, new fields. Write clients that
ignore what they don't know. Fields may be removed or renamed between
versions; the changes are noted in CHANGELOG.md. `hello.version` (or
`GET /api/version`) says which version you're talking to, and `hello.api`
(`/api/version`'s `api`) the interface API version an `interface.json`'s
`api` is checked against: an interface that needs a newer one isn't
installed.

Since 0.2, `spectrum` and the control channel `log` lines come only to
connections that `subscribe` to them.

The **browser version** of Trunk Recorder Pro (WebAssembly, no server) speaks
the same messages between its page and a worker, but has no network API for
other pages to use.
