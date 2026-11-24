# Plugin protocol

The wire protocol between the recorder and a plugin, message by message, for writing a plugin in a
language other than Rust. The Rust SDK implements all of it, and its
[`protocol`](https://docs.rs/trunk-recorder-plugin/latest/trunk_recorder_plugin/protocol/index.html)
module (`crates/trunk-recorder-plugin/src/protocol.rs`) is the reference. How a plugin runs, and
why, is in [Writing plugins](../../plugins/writing-plugins.md#how-a-plugin-runs); the events are
in [Plugin events](events.md).

This is plugin API version **1** (`API_VERSION`).

## Transport

- The recorder runs the plugin's executable **with no arguments**, with the plugin's data folder
  as its working directory (and, on Linux and macOS, in a process group of its own).
- The recorder writes to the plugin's **stdin** and reads its **stdout**. Each message is one JSON
  object on one line, UTF-8, ending in `\n`, with a `type`.
- The plugin's **stderr** is read line by line into its log, and so is a line on stdout that isn't
  a protocol message.
- **Both sides ignore** message types and fields they don't know. New fields and types can appear
  within an API version; removing or changing one needs a new version.

## `--describe`

Run with the single argument `--describe`, a plugin prints its manifest as one JSON object on
stdout and exits 0. It has 10 seconds; one that runs out is asked again, for 30.

```json
{
  "id": "openmhz",
  "name": "OpenMHz",
  "version": "1.0.0",
  "description": "Uploads calls to OpenMHz.",
  "api": 1,
  "subscribe": ["call.concluded"],
  "audio_formats": ["m4a"],
  "config": { "type": "object", "properties": { … }, "x-order": [ … ] },
  "system_config": { "type": "object", "properties": { … }, "x-order": [ … ] },
  "system_overrides": ["server"],
  "site_overrides": ["apiKey"],
  "repository": "https://github.com/TrunkRecorder/trunk-plugin-openmhz",
  "authors": ["…"],
  "license": "GPL-3.0-or-later"
}
```

| Field | Required | |
|---|---|---|
| `id` | yes | `[a-z0-9][a-z0-9-]*`. The registry key and the install folder |
| `name`, `version`, `description` | yes | The display name, the plugin's semver, one sentence for the plugin store |
| `api` | yes | The plugin API version it speaks. The recorder refuses a manifest with none, or a higher one than it knows |
| `subscribe` | yes | Topics: `call.start`, `call.update`, `call.end`, `call.concluded`, `unit`, `audio`, `voice`, `status`, `totals`, `tone_pattern`, `log` ([Plugin events](events.md)) |
| `audio_formats` | no | `["m4a"]` to get M4A with `call.concluded`; add `"wav"` to have the WAV written as well |
| `config`, `system_config` | no | [Settings schemas](#settings-schemas): the plugin's own settings, and those it takes for each system. Leave them out for none |
| `system_overrides` | no | Top-level keys of `config` a system may set again for itself (another server, say). None: every system has the plugin's settings |
| `site_overrides` | no | Top-level keys of `system_config` a site of a system may set again for itself (an account for that site). None: a system's sites have its settings |
| `homepage`, `repository`, `authors`, `license` | no | |
| `page` | no | `{ "title": "…" }`: a page of its own in the recorder's navigation, for settings too many for its card. An empty title is the plugin's name |
| `contribute` | no | Buttons elsewhere in the interface: `[{ "on": "signal", "label", "hint", "append", "item" }]`. Pressed on one of a system's signals in Setup, it adds `item` to the settings array `append`, with `{system}`, `{signal}` (`system:ID`), `{id}` and `{name}` filled in (a string that is just one placeholder inside an array becomes that value), and opens the plugin's page |
| `live_settings` | no | `true`: it takes new settings as it runs ([`settings`](#settings)) instead of being restarted |

The Rust SDK also answers `--version` with the version alone.

## Recorder → plugin

The first message is always `hello`, once.

### `hello`

```json
{
  "type": "hello",
  "api": 1,
  "host": { "name": "trunk-pro", "version": "0.5.0" },
  "config": { "server": "https://api.openmhz.com" },
  "systems": [
    { "index": 0, "id": 1, "short_name": "county", "kind": "p25", "name": "County Public Safety",
      "fcc_call_sign": "WPXT459", "fcc_name": "COUNTY OF EXAMPLE",
      "fcc": { "851012500": { "call_sign": "WPXT459", "name": "COUNTY OF EXAMPLE" } },
      "sites": [{ "id": 1, "name": "north" }, { "id": 4, "name": "south", "config": { "apiKey": "…" } }],
      "config": { "apiKey": "…" } },
    { "index": 1, "id": 2, "short_name": "city", "kind": "p25", "sites": [{ "id": 2, "name": "city" }],
      "config": { "apiKey": "…" }, "settings": { "server": "https://openmhz.example.org" } },
    { "index": 2, "id": 3, "short_name": "conv", "kind": "conventional", "config": null }
  ],
  "capture_dir": "/home/me/TrunkRecorderPro",
  "data_dir": "/home/me/.config/trunk-pro/plugin-data/openmhz",
  "audio_formats": ["wav", "m4a"]
}
```

| Field | |
|---|---|
| `api` | The recorder's protocol version |
| `host` | The recorder's `name` and `version` |
| `config` | The plugin's settings as the user entered them; `null` when there are none |
| `systems` | The systems calls can come from, conventional ones included, but not those the user turned the plugin off for: nothing about those is sent |
| `capture_dir` | Where calls are stored. Always a full path, as are the paths of each call's files |
| `data_dir` | The plugin's own folder, kept across restarts and upgrades (queues, state) |
| `audio_formats` | What `call.concluded` will carry this run: always `wav`, plus the formats the plugin asked for that the recorder can make |

Each system:

| Field | |
|---|---|
| `index` | The `system` events carry for it, for this run only: from 0, the trunked systems then the conventional ones |
| `id` | Its identity, the same across runs and renames; events carry it as `system_id` |
| `short_name` | Unique among the recorder's systems, and what users know it by; it changes when the system is renamed |
| `kind` | `p25`, `smartnet`, `dmr`, `nxdn`, `edacs` or `conventional` |
| `name` | What people call it. Optional, and often missing |
| `fcc`, `fcc_call_sign`, `fcc_name` | Its FCC licenses by frequency (Hz, as a string: `call_sign`, `name`), and the call sign and licensee that name it. Optional, and often missing |
| `sites` | Trunked systems only, not those the plugin is off for: each site's `id` (permanent, as a system's; events carry it as `site_id`), its `name` (events carry it as `site`), and, when the site sets some of the keys `site_overrides` names, `config`: the system's `config` with those changes |
| `config` | The plugin's settings for this system, or `null` |
| `settings` | When it's there: the plugin's `config` with the changes this system makes to the keys `system_overrides` names. Without it, `config` applies to the system as it is |

The plugin answers [`ready`](#ready) when it's running. If it can't run with these settings, it
sends a `status` of `error` saying why, and exits with status **78**.

### Events

`call.concluded`, `call.start`, `call.update`, `call.end`, `unit`, `audio`, `voice`, `status`,
`totals`, `tone_pattern` and `log`, each only to a plugin that subscribed to it. Their fields are
in [Plugin events](events.md).

### `action`

```json
{ "type": "action", "request": 7, "action": "test", "value": { "name": "Discord", "url": "…" } }
```

Someone pressed one of the plugin's buttons (`x-actions` in its settings schema). `action` is the
button's `id`; `value` is the settings object the button is on (an item of an array, or the whole
settings), as edited, saved or not. Answer with [`action.result`](#actionresult) and the same
`request` within 15 seconds; the answer is shown by the button, in the browser that pressed it.
Only a plugin that has actions is sent one.

### `settings`

The plugin's settings changed while it runs: the `hello` it would start with now, with
`"type": "settings"`. Only a plugin with `live_settings` is sent one; others are restarted. A
plugin that can't take them reports an `error` status and keeps the ones it had.

### `calls`

```json
{ "type": "calls", "request": 3, "entries": [{ "path": "county/2026/9/30/101-1790771550_857587500", "record": { "call_num": 412, "short_name": "county", "talkgroup": 101, … }, "audio": true, "json": true }], "next": "1790771550000:county/2026/9/30/101-1790771550_857587500" }
```

The answer to a [`calls.query`](#callsquery), with its `request`: a page of recorded calls, newest
first (by start, then path).

| Field | |
|---|---|
| `entries` | Each call's `path`, its `record` (the call JSON), and whether its audio (`audio`) and `.json` file (`json`) are still kept |
| `next` | There when more calls remain: send it back as the next query's `cursor`. Opaque |
| `error` | Why the answer is only the recent calls (the recorder's call store is off) |

### `settings.saved`

```json
{ "type": "settings.saved", "request": 4, "ok": false, "message": "saving would restart it: its systems have settings of their own" }
```

The answer to a [`settings.save`](#settingssave), with its `request`: saved (`ok`), or `message`
saying why not. Saved, the new settings came first, as [`settings`](#settings).

### `shutdown`

```json
{ "type": "shutdown", "grace_s": 10 }
```

The recorder is stopping, or restarting the plugin. Finish or save what's in progress, then exit.
The recorder closes stdin after this message, and kills the process if it's still running
`grace_s` seconds later. Treat stdin closing without a `shutdown` the same way.

## Plugin → recorder

### `ready`

```json
{ "type": "ready" }
```

It read `hello` and is running.

### `log`

```json
{ "type": "log", "level": "info", "message": "Uploading calls for county" }
```

`level` is `error`, `warn`, `info` or `debug`. All but `debug` go to the plugin's log in the
interface and to the recorder's log; `debug` is dropped.

### `status`

```json
{ "type": "status", "state": "warning", "message": "OpenMHz isn't answering; 12 calls waiting" }
```

`state` is `ok`, `warning` or `error`. It's shown on the plugin's card until the next status.

### `notice`

```json
{ "type": "notice", "key": "token", "state": "warning", "message": "The API key was refused: check it in its settings" }
```

Something that stays true until the plugin says otherwise (a setting to fix), shown on its card
under the status. A later `status` doesn't hide it, and the plugin's health is the worst of its
status and its notices. A notice with the same `key` replaces it; one with no `state` takes it
away; all go when the plugin stops or restarts. (Since SDK 0.16.0.)

### `call.result`

```json
{ "type": "call.result", "path": "county/2026/9/30/101-1790771550_857587500", "outcome": "ok", "message": "", "url": "https://openmhz.com/system/county?call=…", "took_ms": 840 }
```

| Field | |
|---|---|
| `path` | The `path` of the `call.concluded` |
| `outcome` | `ok`, `skipped` (not handled, on purpose) or `failed` (given up on) |
| `message`, `url` | Optional: why, and where the call can be found now |
| `took_ms` | Optional: how long the work on the call took (an upload's last try), so the dashboard tells uploading from waiting |

Send one for every call: see [What becomes of a call](../../plugins/writing-plugins.md#what-becomes-of-a-call).

### `metrics`

```json
{ "type": "metrics", "queued": 12, "retrying": 12, "inFlight": 0, "retries": 30, "bytesSent": 5242880, "latencyMs": 640.0,
  "lastOk": 1790771500.0, "lastError": 1790771555.0, "lastErrorText": "HTTP 503",
  "endpoints": [{ "name": "OpenMHz", "state": "down", "lastError": "HTTP 503" }], "extra": { "listeners": 3 } }
```

How the plugin's work is going, for the Health tab. The field names are camelCase, unlike the
rest of the protocol. Every field is optional; totals are since the plugin started. Send it when
something changes, at most every few seconds.

| Field | |
|---|---|
| `queued` | Calls waiting to be worked on (ready, or waiting to retry) |
| `retrying` | Of those, waiting to retry after a failure |
| `inFlight` | Being worked on now |
| `retries` | Tries that failed and will be retried, in total |
| `bytesSent` | Bytes sent, in total |
| `latencyMs` | How long one piece of work (an upload) takes, smoothed |
| `lastOk`, `lastError`, `lastErrorText` | Unix s of the last success and the last failure, and what the failure was |
| `endpoints` | The services it talks to: each one's `name`, `state` (`unknown`, `up`, `degraded`: answering, with failures, or `down`), and optionally `latencyMs`, `lastOk`, `lastError` |
| `extra` | Anything else worth showing, by name, shown as it is |

### `calls.query`

```json
{ "type": "calls.query", "request": 3, "system": "county", "talkgroup": 101, "limit": 10 }
```

Recorded calls, asked of the recorder: answered with [`calls`](#calls) and the same `request`.
Every field but `request` is optional, and each narrows the calls:

| Field | |
|---|---|
| `system` | A system's short name (its sites' old names count as it). Talkgroup and radio numbers are a system's own: without one, each system's |
| `talkgroup` | Calls on it |
| `unit` | Calls that radio talked on |
| `from`, `to` | Unix ms: calls that started at or after `from`, and before `to` |
| `cursor` | The last page's `next`, for the page after it |
| `limit` | Calls on a page, 1 to 500; 100 when left out |

The recorder answers from its call store (the calls it indexes, `storage.retentionDays` of them),
the latest calls included; with the store off, from the recent calls alone. Ask this way rather
than opening the recorder's database: its tables change between versions.

### `settings.save`

```json
{ "type": "settings.save", "request": 4, "settings": { "serverName": "County Fire" } }
```

Save some of the plugin's own settings, as if typed in on its form: each key of `settings`
replaces that setting (`null` removes it), and the rest stay. The recorder writes them to its
config, sends them back as [`settings`](#settings), and answers with
[`settings.saved`](#settingssaved). It refuses when the save would restart the plugin: the plugin
has to have `live_settings`, run as one copy, have no per-system settings, and keep every setting
it needs set. See [Saving its own settings](../../plugins/writing-plugins.md#saving-its-own-settings).

### `action.result`

```json
{ "type": "action.result", "request": 7, "ok": true, "message": "Sent a test to Discord" }
```

The answer to an [`action`](#action), with its `request`.

## Exit status

| Status | Meaning | The recorder |
|---|---|---|
| 0 | Stopped as asked | Nothing |
| 78 (`EXIT_CONFIG`) | Can't run with these settings | Shows the error, and doesn't restart it until its settings change or recording next starts |
| Anything else, or a signal | Crashed | Restarts it after 1 s, doubling to at most 60 s, back to 1 s once it has run for a minute |

## Delivery

Each plugin has a queue of 1024 messages. What happens when it's full, and what's never dropped, is
in [Order and loss](events.md#order-and-loss).

## Settings schemas

`config` and `system_config` are [JSON Schema](https://json-schema.org) objects. The recorder's
settings form understands this subset. In Rust the SDK makes them from your settings structs
([Settings](../../plugins/writing-plugins.md#settings)); an extension goes on a field with
`#[schemars(extend("x-ref" = "destinations"))]`, or on a struct used as an array's items with the
same attribute on the struct.

| Schema | Drawn as |
|---|---|
| `{"type": "object", "properties": {…}, "x-order": [keys]}` | A group of fields, in `x-order` (else in key order) |
| `{"type": "string"}` | A text box |
| `…, "x-secret": true` | A password box: hidden as it's typed, behind **Show** |
| `…, "format": "uri"` | A text box that checks for a URL |
| `…, "x-multiline": true` | A text area |
| `{"type": "integer"}`, `{"type": "number"}` | A number box; `minimum` and `maximum` apply |
| `{"type": "boolean"}` | A switch |
| `{"enum": [...], "x-enum-labels": [...]}` | A menu; the labels are what it shows |
| `{"type": "array", "items": {"type": "string"}}` (or numbers) | A comma-separated list |
| `{"type": "array", "items": {"type": "object", …}}` | Groups, added and removed one by one |
| `{"type": "array", "items": {"enum": [...]}}` | The choices, to tick |
| `"x-system": true` on a string | A menu of the recorder's systems (short names); follows renames |
| `"x-systems": true` on an array of strings | The recorder's systems, to tick; follows renames |
| `"x-talkgroups": true` on an array of strings | Talkgroups, typed as everywhere in the app (spaces, commas or semicolons between; `1000 - 1099` one range), and each checked. The SDK's `Talkgroups` sets it |
| `"x-signals": true` on an array of strings | Every system's [signals](../../guides/signals.md), to tick, as `system:ID` |
| `"x-ref": "<list>"` | Items of the top-level array `<list>` (by their `id`, else their `name`): a menu on a string, items to tick on an array. Renaming an item renames what names it |
| `"x-tones": true` on an array of objects | Tones to type in, each `pattern`, `tonesHz`, `digits`, `knox`, `lengthsS` and `extraTolerancePct` (a [signal](../../configuration/systems.md#signals)'s fields), with ▶ and a note of what the recorder would match within |
| `"x-summary": ["{field}", "{field\|fallback}", …]` on an object in an array | Its line in the list: each segment with its fields read as the form shows them (a choice by its label), left out when a field in it is empty (and has no fallback) or doesn't apply |
| `"x-title": "<field>"` on an object in an array | Each item is titled by that field and opened to edit, one at a time, rather than all shown at once |
| `"x-when": {"<field>": <value or [values]>}` | Shown only while the object's other fields are so (a webhook's URL only for `"type": "webhook"`); hidden, it keeps its value |
| `"readOnly": true` | Shown, not edited: what the plugin fills in itself with `settings.save` (a name it looked up). One that's also `"format": "uri"` shows the address with a button that opens it in a new tab and one that copies it |
| `"x-open": "…"` | A read-only web address's open button's label (default "Open") |
| `"x-slider": true` | A number drawn as a slider from `minimum` to `maximum`, in steps of `multipleOf`, its value shown beside it (both bounds needed); saved once it's let go, not at each step |
| `"x-actions": [{"id", "label", "hint"}]` on an object | Buttons under it (an array's item, or the whole settings): pressed, the plugin is sent [`action`](#action) with the object as edited |
| `"x-required": true` | Must be filled in, as `required` on its object |

On any field: `title` (the label; the key spelled out if there's none), `description` (help text
under it) and `default`.

On an object: `required`, the keys that have to be filled in. Until they are, the recorder shows
the plugin (for `config`) or that system (for `system_config`) as not set up. It doesn't stop the
user saving without them, so a plugin still checks.

The form doesn't draw maps or enums that carry data.
