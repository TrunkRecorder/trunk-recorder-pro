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

A running recorder serves all of this:

| URL | What |
|---|---|
| `/` | The home interface: the built-in one, or yours ([Serving your interface](#serving-your-interface)) |
| `/builtin/` | The built-in interface, always |
| `/ui/` | Every interface, the examples and these docs, as links |
| `/ui/<name>/` | Your interface `<name>` |
| `GET /api/docs` | This page (Markdown) |
| `GET /api/llms.txt` | The LLM guide |
| `GET /api/client.js` | The client library ([client.js](#clientjs)) |
| `GET /api/examples/…` | The examples, runnable: e.g. `/api/examples/wall.html` |
| `GET /api/protocol.ts` | Every message, as TypeScript types with comments: **the definition** |
| `GET /api/schema` | The same as JSON Schema (draft-07): `definitions.FromRecorder` / `definitions.ToRecorder` |
| `GET /api/interfaces` | The interfaces configured, and any problem with each (`InterfacesInfo`) |
| `GET /api/version` | `{"app": "trunk-pro", "version": "…"}` |
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
14. [Compatibility](#compatibility)

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
trunk-pro --ui ./my-folder      # your page at http://localhost:8080/, the built-in one at /builtin/
```

Edit the file and reload: the recorder serves the folder as it is on disk.

Without the library, the protocol is plain JSON over a WebSocket. This prints
each call as it's recorded, in Node 22+ or a browser:

```js
const ws = new WebSocket("ws://localhost:8080/api/ws");
ws.onmessage = (ev) => {
  if (typeof ev.data !== "string") return; // live audio (binary), not asked for here
  const m = JSON.parse(ev.data);
  if (m.type === "hello") console.log(`Connected to ${m.version}; recorder is ${m.phase.phase}`);
  if (m.type === "concluded") {
    const r = m.entry.record;
    console.log(`${r.talkgroup_tag || r.talkgroup} on ${r.short_name}: ${r.call_length} s`);
    console.log(`  audio: http://localhost:8080/calls/${m.entry.path}.wav`);
  }
};
```

## Serving your interface

The simplest way to run an interface is to let the recorder serve it. Its
pages are then on the recorder's own origin, so they need no access settings,
and `/api/client.js`, `/api/ws` and `/calls/` are right there.

An interface is **a folder with an `index.html`**, plus whatever else it uses:
scripts, styles, images, more pages. There's no build step unless you want
one. A bundler's output folder (`dist/`) works too; build with relative
paths (Vite: `base: "./"`).

**In the config** (Setup → Recording → Interfaces, or the config file):

```json
"server": {
  "interfaces": [
    { "name": "wall", "path": "/home/me/wall-display" },
    { "name": "scanner", "path": "ui/scanner" }
  ],
  "home": "wall"
}
```

- Each interface is served at `/ui/<name>/`. A name is letters, digits, `-`
  and `_`.
- `path` is the folder: absolute, `~/…`, or relative to the config file's
  folder.
- `home` says what `/` shows: `""` (the default) for the built-in interface,
  or an interface's name. The built-in interface is at `/builtin/` either way,
  so setup is always one link away.
- Changes take effect at once. No restart needed.

**For one run:** `trunk-pro --ui <folder>` shows that folder at `/`, over
`home`. Nothing is saved, which makes it handy while developing.

**How files are served:**
- `/ui/wall/` is the folder's `index.html`, and `/ui/wall/app.js` its
  `app.js`. A subfolder serves its own `index.html`.
- A path with no extension that isn't a file (`/ui/wall/settings`) gets the
  folder's `index.html`. Pages that route themselves this way work; hash
  routes (`#/settings`) work anywhere.
- Files are read on every request, with `Cache-Control: no-cache`: edit, then
  reload.
- Nothing outside the folder is served, symbolic links included.

`/ui/` lists every interface (with problems, such as a missing folder), the
examples, and these docs. `/api/interfaces` gives the same as JSON.

## client.js

`/api/client.js` (in the repository: `docs/api/client.js`) is an ES module
with no dependencies. It works in browsers and in Node 22+. You don't have to
use it, since the protocol is simple, but it takes care of the fiddly parts:
reconnecting, re-sending `listen` after a reconnect, keeping state, safe
config edits, and gapless audio.

```js
import { TrunkClient, LivePlayer, decodeAudioFrame, parseTalkgroups, CONVENTIONAL, conventionalIndex, duration, mhz, escapeHtml } from "/api/client.js";
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
| `config` | the config (`Config`) |
| `phase` | `{ phase: "idle" \| "starting" \| "running" \| "stopping", error, ended }` |
| `status` | `EngineStatus` while running: `nowS`, totals, `systems[]` |
| `sources` | `SourceStatus[]`: each radio |
| `calls` | `CallView[]`: on the air now (empty when not running) |
| `history` | `CallEntry[]`: recorded calls, newest first |
| `log` | the latest 500 `LogLine`s |
| `unitCsv`, `aliases` | unit names as saved, and talker aliases heard (see `unitName`) |
| `error` | the latest `error` message's text |
| `exited` | the recorder said it was exiting (`quit`); it keeps trying to reconnect, and this clears when it's back |

**Events.** `rec.on(type, fn)` returns a function that removes the handler.

| `type` | `fn` gets | When |
|---|---|---|
| `"change"` | the client | after any message: redraw |
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
| `setConfig(edit)` | `edit(copy)` changes a copy of the latest config, and the whole config is sent |
| `callUrl(entry, "wav" \| "json" \| "m4a")` | a recorded call's file URL |
| `talkgroups(shortName)` | the system's talkgroup file: `Map` talkgroup → `{ talkgroup, alphaTag, description, tag, group, mode }` |
| `unitName(shortName, unit)` | a radio's name: the unit names file (regular-expression rows too), else the alias heard, else `""` |
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
talkgroup CSV. `conventionalIndex(system)` turns a
conventional system's `system` number into its place in
`config.conventional`.

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
  origin with each request. The recorder accepts its own pages (including
  every interface it serves) and anything that isn't a browser (no `Origin`
  header: scripts, Node, curl). It answers any other origin with **403** and
  logs `Refused a page from …` once.

To use the API from a page the recorder **doesn't** serve (a dev server, a
page on another machine, a file), add the page's origin to `allowedOrigins`
in the `server` section of the config, or under Setup → Recording →
Interfaces:

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
2. **`hello`** arrives first. It holds everything to draw the first screen:
   the config, the recorder's phase, the last 300 recorded calls, the radios
   plugged in, and the survey's state.
3. **`plugins`** comes next (the plugins installed).
4. Then **messages as things happen**, to every connected client:
   - `state` when recording starts or stops
   - `status` about twice a second while recording: systems, sources and the
     calls on the air
   - `spectrum` for each radio, ~7 times a second
   - `concluded` for each recorded call
   - `log` lines
   - `config` when anyone changes the config
   - …the full list is [below](#messages-from-the-recorder).
5. **Send commands** as JSON text whenever you like. Answers to your own
   commands (`error`, `notice`, `dir`, `trConfig`, `devices`, `radios`,
   `plugins`, `pluginStore`) come to your connection only. Everything else goes to every
   client.
6. **Reconnect** when the socket closes: the recorder may have restarted. A
   new `hello` brings you up to date. Per-connection settings (live audio)
   must be sent again after it.
7. **`quit`** means the recorder is exiting (Quit, Ctrl-C, a service stop or
   restart). The built-in interface stops there. A display that should come
   back after a restart keeps trying, more slowly. client.js retries every
   5 s and sets `exited`.

Unknown message types should be ignored: new ones may be added.

## Concepts

**Phase.** The recorder is `idle`, `starting`, `running` or `stopping`.
`hello.phase` and the `state` message carry it, with `error` (why it stopped
or couldn't start) and `ended` (a capture file played to its end).

**Systems.** `config.systems` are trunked systems: P25, SmartNet and DMR
sites. Each site recorded from its own control channel is its own system with
its own `shortName`, which is also the folder its calls are saved in. While
running, `status.status.systems[i].index` is the number that calls and live
audio carry as `system`. It is the system's position among the systems being
**recorded** (enabled, with a control channel), so match on `shortName`
rather than on positions in the config.

**Conventional systems.** `config.conventional[k]` are groups of fixed
frequencies. Their calls carry `system` = `65535 - k` (`conventionalSystem(k)`
in protocol.ts). Each frequency acts as a talkgroup: the channel's
`talkgroup`, or by default the frequency in kHz.

**Calls on the air vs. recorded calls.**
- `status.calls` (`CallView[]`) are the calls happening right now. That
  includes ones being followed but not recorded: `state: "monitoring"` with a
  `reason` such as `"encrypted"` or `"unknown_tg"`.
- A recorded call arrives once, finished, as `concluded`. Its `entry` is a
  `CallEntry`: `path` (where its files are) and `record`, Trunk Recorder's
  call JSON (`CallRecord`). The same entries make up `hello.history`, newest
  first.

**Talkgroups** are numbers. Their names (`alphaTag` / `talkgroup_tag`) come
from each system's talkgroup file (`talkgroupsCsv` in the config), and are
`""` when the talkgroup isn't in it. **Units** (radios) are numbers too:
`CallView.sources`, `CallRecord.srcList`. Their names come from
`hello.units` (CSV per system), the `unitAlias` messages (talker aliases
heard on the air), and `srcList[].tag`.

**Times.** Engine times (`nowS`, `startS`, `timeS`) are seconds since
recording started. Times in call records are Unix seconds / milliseconds.
Frequencies are Hz everywhere.

## Messages from the recorder

Every message is a JSON object with a `type`. Exact fields are in
`FromRecorder` in protocol.ts.

| `type` | When | What's in it |
|---|---|---|
| `hello` | First, on every connection | `version`, `platform`, `config`, `configPath`, `devices`, `phase`, `history` (≤ 300 `CallEntry`, newest first), `units` (short name → unit names CSV), `heard`, `radios`, `surveyBands`, `survey` |
| `plugins` | After `hello`; after plugin changes; answers `plugins` | `plugins` (`PluginInfo[]`), `encoderFound` |
| `state` | Recording starts / stops | `phase`, `error`, `ended` |
| `status` | ~2/s while running | `status` (`EngineStatus`: totals, `systems[]` with control channel, decode counts `good`/`bad`, site identity, patches, DMR state), `sources[]` (each radio: rate, drops, errors, frequency error), `load` (share of real time the decoder is busy), `calls` (`CallView[]`, on the air now) |
| `spectrum` | ~7/s per radio while running | `source`, `centerHz`, `rateHz`, `bins`: 512 levels in dBFS, lowest frequency first, spanning `centerHz ± rateHz/2` |
| `concluded` | A call was recorded | `entry`: `{ path, record }` |
| `log` | Things happened | `lines[]`: `{ timeS, kind, text, system? }` — `kind` e.g. `"error"`, `"control"`, `"alias"`, `"plugin"`, or a control-message kind |
| `unitAlias` | A talker alias was heard | `system` (short name), `unit`, `alias` |
| `heard` | Conventional codes heard changed | `heard`: frequency (Hz, as a string) → `HeardCode[]` |
| `config` | The config changed (anyone's `setConfig`, a plugin added…) | `config` |
| `devices` | Answers `devices` | `devices`: RTL-SDR dongles plugged in (`busy` when another program has one) |
| `radios` | Answers `findRadios` | `radios`: the optional USRP / Airspy / SoapySDR drivers and their devices |
| `dir` | Answers `listDir` | a folder: `path`, `parent`, `dirs`, `files` (.json), `home`, `sep`, `error` |
| `trConfig` | Answers `readTrConfig` | a Trunk Recorder config.json's `text`, the `files` it names, or `error` |
| `survey` | The survey's progress | `stage` (`idle` / `scanning` / `monitoring` / `done`), candidates, the channel monitored, the suggested system |
| `surveySpectrum` | While surveying | like `spectrum` |
| `pluginRuntime` | A plugin's totals changed | `id`, `runtime` (state, counts, recent log) |
| `pluginState` | A plugin reports how it is | `id`, `state` (`ok` / `warning` / `error`), `message` |
| `pluginResult` | A plugin finished with a call | `id`, `path`, `outcome` (`ok` / `skipped` / `failed`), `message`, `url` |
| `pluginStore` | Answers `pluginStore` | the plugin registry's listings |
| `pluginInstall` | An install's progress | `key`, `id`, `stage` (`finding` … `done` / `failed`), `message` |
| `error` | A command failed | `message`, for the user |
| `notice` | A command worked and has something to say | `message` |
| `quit` | The recorder is exiting (it may be restarted) | — |

## Commands to the recorder

Send JSON text. Exact fields are in `ToRecorder` in protocol.ts. Commands that
aren't understood are ignored.

| `type` | Fields | Does |
|---|---|---|
| `start` | — | Start recording with the saved config. `state` follows; `error` if it can't. |
| `stop` | — | Stop recording. Calls in progress are saved. |
| `listen` | `on`, `system` (a short name, or null), `talkgroup` (number or null) | Live audio on this connection: every call (both null), one system's, one talkgroup's, or both. `on: false` stops it. |
| `setConfig` | `config` | Replace the whole config: saved, and sent to everyone as `config`. Takes effect at the next `start` (plugin settings at once). |
| `channelFile` | `index`, `path` | Link conventional system `index`'s channels to a CSV, reload it (same path), or unlink (`""`). |
| `devices` | — | List RTL-SDR dongles (→ `devices`). |
| `findRadios` | — | Look for USRP / Airspy / SoapySDR devices (→ `radios`; takes a few seconds). |
| `listDir` | `path` (`""` = home) | List a folder on the recorder's computer (→ `dir`). |
| `readTrConfig` | `path` | Read a Trunk Recorder config.json and its files, to import (→ `trConfig`). |
| `surveyStart` | `source`, `bands` (ids from `hello.surveyBands`), `findGain` | Scan for systems with radio `source` (not while recording). |
| `surveyListen` | `freqHz` | Monitor this signal. |
| `surveyRescan` / `surveyStop` | — | Scan again / end the survey. |
| `plugins` | — | List plugins again (→ `plugins`). |
| `addPlugin` | `path` | Add a plugin executable of your own. |
| `removePlugin` | `id` | Remove a plugin. |
| `pluginStore` | `refresh?` | The plugin registry's list (→ `pluginStore`). |
| `installPlugin` | `id`, or `repository` + `tag?` | Install or update a plugin (→ `pluginInstall` as it goes). |
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

Each recorded call's files are under the recordings folder at
`CallEntry.path`:

- `/calls/<path>.wav`: the audio, 8 kHz mono
- `/calls/<path>.json`: the same `CallRecord` that came in `concluded`
- `/calls/<path>.m4a`: when `recording.compressWav` is on

URL-encode each segment of the path:
`path.split("/").map(encodeURIComponent).join("/")`. An `<audio>` element can
play the `.wav` directly. The files can be deleted after their plugins have
uploaded them (`recording.audioArchive: false`), so older calls in the
history may have no files.

`CallRecord` is Trunk Recorder's call JSON, so tools written for Trunk
Recorder's files read it too. The fields you'll want most: `talkgroup`,
`talkgroup_tag`, `short_name`, `start_time` (Unix s), `call_length` (s),
`freq`, `emergency`, `encrypted`, `srcList` (the radios that talked, with
names in `tag`), and the reception fields `signal` / `noise` / `snr` (dB).

## Recipes

Each recipe shows the raw protocol, then the client.js equivalent.

**Show what's on the air.** Redraw from each `status`: `m.calls` is the
complete list each time. A call with `state: "recording"` is being recorded.
With client.js: `rec.calls`, on `change`.

**How long a call has been going.** `status.nowS - call.startS` (both engine
seconds). With client.js: `rec.status.nowS - c.startS`.

**A call log.** Start from `hello.history` and put each `concluded.entry` at
the front. It's the same shape either way. With client.js: `rec.history`.

**Listen to one talkgroup.** Send `{"type": "listen", "on": true, "system":
null, "talkgroup": 1234}`. Add `system` when talkgroup numbers repeat across
systems. With client.js: `rec.listen({ talkgroup: 1234 })`. For several
talkgroups, listen to everything and drop the chunks you don't want.

**Play new recordings one after another** (no live audio needed): on each
`concluded`, queue `/calls/<path>.wav`, and play the queue with one `<audio>`
element. `examples/talkgroup.html` does this.

**Change a setting.** Take the latest config (from `hello`, or the latest
`config` message), change it, and send the **whole** object back with
`setConfig`. Then wait for the `config` message that everyone gets. Never
build a config from scratch: fields you leave out are reset to defaults.

```js
const next = structuredClone(config);
next.recording.minCallS = 2;
send({ type: "setConfig", config: next });
// client.js:
rec.setConfig((c) => { c.recording.minCallS = 2; });
```

**Start and stop.** Send `start` / `stop` and draw from `state`. Don't assume
the command worked: `error` says why it didn't.

**A talkgroup's name.** In a call: `alphaTag` (live) or `talkgroup_tag`
(recorded). Otherwise, each system's talkgroup file is in
`config.systems[i].talkgroupsCsv` (Trunk Recorder's CSV format). With
client.js: `rec.talkgroups(shortName).get(1234)?.alphaTag`.

**A radio's name.** In a recorded call: `srcList[].tag`. Live: look the unit
up in `hello.units[shortName]` (headerless `unit,name` CSV; a unit between
slashes is a regular expression), then in the `unitAlias` messages heard.
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
3. Open `http://localhost:8080/`, check it, and iterate.

The recorder serves the files as they are on disk, so a reload shows each
change.

## Compatibility

protocol.ts is the definition. The recorder's tests check what it actually
sends against the schema generated from protocol.ts: every message type in
both directions, and the shapes of the main messages (`hello`, `status`,
`concluded`, `config`, the survey's, the plugins'…). So this page and the
types stay true to the recorder you're running.

The protocol can grow: new message types, new fields. Write clients that
ignore what they don't know. Fields may be removed or renamed between
versions; the changes are noted in CHANGELOG.md. `hello.version` (or
`GET /api/version`) says which version you're talking to.

The **browser version** of Trunk Recorder Pro (WebAssembly, no server) speaks
the same messages between its page and a worker, but has no network API for
other pages to use.
