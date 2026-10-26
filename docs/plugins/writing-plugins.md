# Writing plugins

This page is an overview of building a plugin of your own: starting from the template, the
protocol and a plugin's lifecycle, the Rust SDK, testing, and publishing to the registry. The
template's guide and the SDK's API reference have the full detail; this page links to them.

To use plugins rather than write them, see [Plugins](README.md).

## What you're building

A plugin is a program of its own. The recorder runs it while recording, writes events to its stdin
as JSON lines, and reads its answers from its stdout. It can be written in any language; the
[Rust SDK](#the-rust-sdk), the crate [`trunk-recorder-plugin`](https://crates.io/crates/trunk-recorder-plugin),
does the plumbing for you, and the template is built on it.

Plugins only watch. They can't change what's recorded; they can log, report their health, and
report what became of each recorded call. Uploaders, streamers, loggers and notifiers are all
plugins.

| Where | What's there |
|---|---|
| [trunk-plugin-template](https://github.com/TrunkRecorder/trunk-plugin-template) | A working example plugin with tests, CI and a release workflow, and its `docs/` guide to writing plugins |
| [docs.rs/trunk-recorder-plugin](https://docs.rs/trunk-recorder-plugin) | The SDK's API reference |
| [The template's protocol page](https://github.com/TrunkRecorder/trunk-plugin-template/blob/main/docs/protocol.md) | The wire format, for plugins in other languages |
| [TrunkRecorder/plugins](https://github.com/TrunkRecorder/plugins) | The registry the plugin store installs from |
| The published plugins ([OpenMHz](https://github.com/TrunkRecorder/trunk-plugin-openmhz), [Broadcastify](https://github.com/TrunkRecorder/trunk-plugin-broadcastify), [Rdio Scanner](https://github.com/TrunkRecorder/trunk-plugin-rdioscanner), [simplestream](https://github.com/TrunkRecorder/trunk-plugin-simplestream), [upload-script](https://github.com/TrunkRecorder/trunk-plugin-upload-script), [MQTT](https://github.com/TrunkRecorder/trunk-plugin-mqtt)) | Complete plugins to learn from |

## Starting from the template

You need Rust ([rustup.rs](https://rustup.rs)) and a Trunk Recorder Pro that has recorded some
calls to test against; no radio needs to be attached while you work.

1. On GitHub, press **Use this template** on
   [trunk-plugin-template](https://github.com/TrunkRecorder/trunk-plugin-template), naming the new
   repository after your plugin (`trunk-plugin-pager`, say). Clone it.
2. Pick your plugin's **id**: lowercase letters, digits and dashes, like `pager`. It names the
   executable, the release files, the install folder and the registry entry, and it can't change
   once people have installed your plugin. Set it as `name` in `Cargo.toml`, along with
   `description`, `repository`, `authors` and `license`. Set the display name in `manifest()` in
   `src/main.rs`.
3. Build it and ask it who it is:

   ```bash
   cargo build
   ./target/debug/pager --describe
   ```

4. Run it against calls you've recorded:

   ```bash
   trunk-pro plugin run ./target/debug/pager ~/TrunkRecorderPro --limit 5
   ```

5. Replace the example (**call-log**, which writes a line of JSON per call) with your plugin,
   keeping `cargo test` passing.

The template's [Getting started](https://github.com/TrunkRecorder/trunk-plugin-template/blob/main/docs/getting-started.md)
walks through this in more detail, including what to put in your README.

## How a plugin runs

The lifecycle, as the recorder (the "host") sees it:

1. **Describe.** When a plugin is installed, added, or about to start, the recorder runs it with
   `--describe` and reads its **manifest**: one JSON object with its id, name, version, the
   protocol version it speaks (`api`), the topics it subscribes to, any extra audio formats it
   wants, and the JSON Schema of its settings. The recorder refuses a manifest with no `api`, or an
   `api` newer than its own (`API_VERSION` is 1).
2. **Start.** When recording starts, the recorder runs the executable with no arguments, in its
   data folder (`plugin-data/<id>/`) as its working folder (in a process group of its own on Linux
   and macOS), and writes `hello`: the
   plugin's settings, every system (short name, kind, the plugin's settings for it, and, when
   the system overrides some of the plugin's own settings, those settings as they are for it:
   `settings`), the recordings folder, the data folder, and the audio formats calls will carry.
3. **Ready.** The plugin answers `ready` once its settings are good. If they aren't, it sends an
   error `status` saying what to fix and exits with status **78**: the recorder shows the message
   and doesn't restart it until its settings change or recording starts again.
4. **Events.** The recorder sends the events the plugin subscribed to, and nothing else; events
   nobody subscribed to aren't even built.
5. **Shutdown.** When recording stops (or the plugin's settings change), the recorder sends
   `shutdown` with a grace period in seconds (10 when recording stops), then closes stdin. The
   plugin finishes or saves what it's working on and exits. One still running after the grace
   period is killed.

Any other exit is a crash: the plugin is restarted after 1 second, doubling to at most 60 seconds,
and the events queued for it wait. Each plugin's queue holds 1024 messages; a plugin that falls that
far behind loses events (counted on the dashboard) rather than slowing the recorder.

### Topics

| Topic | Message | When |
|---|---|---|
| `call.start` | `CallInfo` | A call starts: talkgroup and its tag, frequency, TDMA slot, analog / encrypted / emergency flags and the grant's duplex / packet / priority, whether it's recorded (and why not), the radios heard so far (`sources`: each with what its own transmission said of itself — emergency, encrypted, duplex, packet, priority), patched talkgroups |
| `call.end` | `CallInfo` | A call ends; its files follow in `call.concluded` |
| `call.concluded` | `ConcludedCall` | A recorded call's files are on disk: its path key, its call JSON (Trunk Recorder's format, as a typed `CallRecord`: every field the recorder writes is a named field) and the paths of its `.json`, `.wav` and, when asked for, `.m4a` |
| `unit` | `UnitEvent` | A radio registered, deregistered, affiliated, acknowledged, reported its location, got a data grant, or sent an answer request or call alert |
| `tone_pattern` | `TonePatternEvent` | A call's tones made a pattern: every page, alert tone, DTMF or KNOX string and Morse ID heard, as soon as it's over (a second after its last tone). The pattern is as the call JSON's `tone_patterns` has it (with `text` for a person, the radio that sent it, and the ids of the system's [signals](../configuration/systems.md#signals) it is), with the call's system, site, `call_num`, talkgroup and alpha tag. One heard in voice decrypted with the recorder's keys (`decrypted`) goes only to plugins with `shareDecrypted` |
| `audio` | `AudioChunk` | Live audio of recording calls: 16-bit mono PCM at 8 kHz, base64 |
| `status` | `Status` | Every system's state every few seconds: control channel, decode rate, active and recording calls |

Every event carries the system's `short_name`, which is its identity. The numeric `system` index is
for the current run only, so store anything by short name. It counts from 0: the trunked systems
being recorded first, then the conventional ones (there is no `CONVENTIONAL` index any more); a
system's `kind` in `hello` (`p25`, `smartnet`, `dmr`, `nxdn`, `edacs` or `conventional`) says which it is. Calls and unit events also carry `site`,
the name of the site they were heard on (a system of one site: its short name), and `status` has an
entry per site, with its `site_name`. A call heard on several sites of a system is one call: one
`call.start` and one `call.end` (from its first copy), and one `call.concluded`, for the copy kept.

### What a plugin sends back

| Message | Meaning |
|---|---|
| `ready` | It read `hello` and is running |
| `log` | A log line at `error`, `warn`, `info` or `debug`. All but debug show in the plugin's log in the interface and in the recorder's log |
| `status` | Its health (`ok`, `warning`, `error`) and a message, shown on its card until the next one |
| `call.result` | What became of a concluded call: `ok`, `skipped` or `failed`, a message, optionally a URL, and optionally `took_ms`, how long your work on it took (an upload's last try) |
| `metrics` | Figures for the Health tab: queue depth, retries, in flight, bytes sent, latency, the services it talks to (up, degraded, down), and any of its own |
| `action.result` | How one of its buttons went (an `action`, below): `ok`, and a message shown beside the button |

Lines on stdout that aren't protocol messages become log lines, and so does everything on stderr.

**If you subscribe to `call.concluded`, send a `call.result` for every call.** The recorder keeps
each call's files until every plugin taking recorded calls has reported on it, then deletes what
the user's **Archive** settings don't keep. `ok` and `skipped` count the same there: the plugin is
finished with the call. Only `failed` matters, and keeps the files when the user has **Keep when a
plugin fails** on. A call your plugin never reports on counts as failed after an hour.

The recorder times every call it gives you, from the call's end to your `ok`, and shows that age
on your card in the Health tab with its parts: how long the user's delay rules held it, and how
long it waited. Send `took_ms` with `ok` (`Host::call_result_took`; `CallQueue` does it for you)
and the card can tell your upload from the waiting. Without it, the card shows the age alone. You
don't measure anything else: the recorder does, the same for every plugin.

### Audio

`call.concluded` always carries a WAV path (16-bit mono, 8 kHz). A plugin that wants M4A lists
`"m4a"` in its manifest's `audio_formats`; the recorder then encodes each call once for every
plugin that asked, when it has an encoder. `hello`'s `audio_formats` says whether M4A will come
this run. When it won't, either fall back to WAV, or fail `start` with a message telling the user
to install ffmpeg. When a call does come with an M4A, its WAV may not have been written: use the
M4A.

## The Rust SDK

Add the crate and `schemars` (for the settings form):

```toml
[dependencies]
trunk-recorder-plugin = "0.7.0"
schemars = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

SDK 0.7.0 is the one for Trunk Recorder Pro 0.2.10. A plugin built on 0.5.0 runs unchanged unless
it subscribes to tone patterns: that topic is `tone_pattern` now (`TonePatternEvent`), a pattern has
`text`, and KNOX digits are `knox: true` rather than a `KNOX ` prefix. Rebuild on it for buttons, a page
of its own, settings taken without a restart and the names in events (below). A plugin built on an
SDK older than 0.5.0 must be rebuilt: systems are numbered from 0 (conventional ones too), and the
call record names every field instead of keeping some in `extra`. A plugin implements the `Plugin` trait and calls `run`
from `main`:

```rust
use trunk_recorder_plugin::{topic, ConcludedCall, Host, Manifest, NoConfig, Outcome, Plugin, Setup};

struct Hello { host: Host }

impl Plugin for Hello {
    type Config = NoConfig;
    type SystemConfig = NoConfig;
    fn manifest() -> Manifest {
        Manifest { name: "Hello".into(), subscribe: vec![topic::CALL_CONCLUDED.into()], ..trunk_recorder_plugin::manifest!() }
    }
    fn start(host: Host, _: Setup<NoConfig, NoConfig>) -> Result<Self, String> {
        Ok(Hello { host })
    }
    fn call_concluded(&mut self, call: ConcludedCall) {
        self.host.info(format!("TG {} for {} s", call.call.talkgroup, call.call.call_length));
        self.host.call_result(&call.path, Outcome::Ok, "", "");
    }
}

fn main() {
    trunk_recorder_plugin::run::<Hello>();
}
```

The main pieces:

| Item | What it does |
|---|---|
| `Plugin` | The trait. `Config` and `SystemConfig` are your settings types (`NoConfig` for none). `manifest()`, `start()`, one method per topic (`call_start`, `call_end`, `call_concluded`, `unit`, `audio`, `status`, `tone_pattern`), `action(a, reply)` (one of its buttons pressed: answer with the `Reply`), `settings(setup)` (new settings while it runs, with `live_settings`) and `shutdown(grace)`. Every method runs on the one thread reading the recorder's messages, so return quickly and hand network or disk work to threads of your own |
| `manifest!()` | A `Manifest` with the id, version, description, homepage, repository, authors and license from `Cargo.toml`; set `name`, `subscribe` and `audio_formats` |
| `run::<P>()` | Answers `--describe` (and `--version`), parses `hello` and your settings, calls your methods, and exits with 78 when `start` returns `Err` |
| `Setup` | What `start` gets: `config`, `systems` (each with `index`, `short_name`, `kind`, `name`, `fcc` (its FCC licenses by frequency), `fcc_call_sign` and `fcc_name` (the one that names it), `sites` (each `SiteInfo { name }`), and your `SystemConfig` or `None`), `host` (`HostInfo { name, version }`: the recorder's), `capture_dir`, `data_dir`, `audio_formats`; `system_named()` and `has_format()`. The name and FCC licenses are optional, so often empty |
| `Host` | The way back, usable from any thread: `info`/`warn`/`error`/`debug`, `status`, `call_result` (and `call_result_took`, with your time on it), `metrics`. Never `println!` in a plugin: stdout belongs to the protocol |
| `Reply` | The answer to a button (`Plugin::action`): `ok(message)` or `fail(message)`, once, from any thread; dropped unsent, it answers that the plugin didn't. The recorder waits 15 s for it |
| `CallQueue`, `QueueOptions`, `Attempt` | An upload queue on worker threads (2 by default) with retries after 10 s, 1 min, 5 min and 15 min, a warning status while calls wait, `call.result`s and `metrics` sent for you, and calls still waiting at shutdown saved to the data folder (`QueueOptions::saved_in`) and resumed next start. Your closure returns `Attempt::Done`, `Skip`, `Retry` or `Fail`. `CallQueue::start_by` also takes a closure naming each call's lane (the server it goes to): each lane gets workers, retries and an endpoint status of its own |
| `Talkgroups` | A system's talkgroups, for your `SystemConfig` (`#[serde(flatten)] talkgroups: Talkgroups`): the **Talkgroups** menu (every one, only some, all but some) and its lists, as the uploaders have it, reading their older settings too. `filter_for(&host, short_name)?` in `start` checks every entry and gives a `TalkgroupFilter` (SDK 0.7.1) |
| `TalkgroupFilter` | Talkgroup allow and deny lists: numbers, ranges (`1000-1099`) and patterns (`507*`, `12?45`); `passes(tg)`. `TalkgroupFilter::parse` and `filter::talkgroup_list` read lists as typed and say which entry isn't a talkgroup; `filter::patterns` deserializes a list that mixes numbers and strings |
| `Multipart` | A `multipart/form-data` body for any HTTP client |
| `testing` | Run a plugin without the recorder (below) |

Four of the six published plugins use `CallQueue` (simplestream, which is live, and MQTT, which has
a queue of its own, don't), and the smaller ones are short enough to read in one sitting.

### Settings for each system

A system can override any of your recorder-wide settings (`Config`) for itself, such as sending its
calls to another server. Read them for a system from `System::settings`, which is your `Config` with
that system's overrides applied, and use `Setup::config` only for what isn't any one system's. An
uploader that builds one client per server and starts its `CallQueue` with `start_by`, keyed by
server, handles it all.

### Settings and the form

Derive `JsonSchema` (and `Serialize`, so defaults appear in the form) on your settings structs, with
`#[serde(default)]` so users can leave anything empty. The recorder draws the settings form from the
schema: a doc comment's first paragraph is a field's label and the rest its help text. `Config` is
drawn on the plugin's card in Setup; `SystemConfig` is repeated on every system's card.

The form understands:

| Schema | Drawn as |
|---|---|
| `string` | A text box; `"x-secret": true` hides what's typed, `"format": "uri"` checks for a URL, `"x-multiline": true` makes a text area |
| `integer`, `number` | A number box; `minimum` and `maximum` apply |
| `boolean` | A switch |
| `enum` | A menu; `"x-enum-labels"` are what it shows |
| array of strings or numbers | A comma-separated list |
| array of objects | Groups added and removed one by one |
| `object` | A group of the above |
| `"x-system": true` on a string | A menu of the recorder's systems; follows renames |
| `"x-required": true` | Must be filled in; until it is, the plugin (or that system) shows as not set up. Mark it with `#[schemars(extend("x-required" = true))]` |
| array whose items are an `enum` | The choices, to tick |
| `"x-systems": true` on an array of strings | The recorder's systems, to tick |
| `"x-talkgroups": true` on an array of strings | Talkgroups, typed as everywhere in the app (spaces, commas or semicolons between; `1000 - 1099` one range), and each checked. `Talkgroups` sets it |
| `"x-signals": true` on an array of strings | Every system's [signals](../guides/signals.md), to tick, as `system:ID` |
| `"x-ref": "<list>"` | Items of the top-level array `<list>` (by their `id`, else their `name`): a menu on a string, items to tick on an array. Renaming an item renames what names it |
| `"x-tones": true` on an array of objects | Tones to type in, each `pattern`, `tonesHz`, `digits`, `knox`, `lengthsS` and `extraTolerancePct` (a [signal](../configuration/systems.md#signals)'s fields), with ▶ and a note of what the recorder would match within |
| `"x-summary": ["{field}", "{field\|fallback}", …]` on an object in an array | Its line in the list: each segment with its fields read as the form shows them (a choice by its label), left out when a field in it is empty (and has no fallback) or doesn't apply |
| `"x-title": "<field>"` on an object in an array | Each item is titled by that field and opened to edit, one at a time, rather than all shown at once |
| `"x-when": {"<field>": <value or [values]>}` | Shown only while the object's other fields are so (a webhook's URL only for `"type": "webhook"`); hidden, it keeps its value |
| `"x-actions": [{"id", "label", "hint"}]` on an object | Buttons under it (an array's item, or the whole settings): pressed, the plugin is sent `action` with the object as edited |

`Option<T>` becomes `T` (empty means not set), unit-variant enums become menus, and nested structs
are inlined. Extensions go on a field with `#[schemars(extend("x-ref" = "destinations"))]`, or on a
struct used as an array's items with the same attribute on the struct. The template's [Settings](https://github.com/TrunkRecorder/trunk-plugin-template/blob/main/docs/settings.md)
page has examples, and advice on changing settings in later versions without breaking existing
installs. Mark API keys and passwords `x-secret`, and never log them.

### More room, buttons, and live settings

A plugin whose settings are more than a few fields (rules and where they send, say) can ask for
more of the interface in its manifest:

| Manifest | What it gets |
|---|---|
| `page: { title }` | A page of its own, in the navigation under the radio pages (`#/plugin/<id>`): its settings full width, its state and its log. Its card in Setup links to it |
| `contribute: [{ on, label, hint, append, item }]` | Buttons elsewhere in the interface. `on: "signal"` puts one on each of a system's signals in Setup; pressed, it adds `item` to the settings array `append` (with `{system}`, `{signal}` (`system:ID`), `{id}` and `{name}` filled in) and opens the plugin's page |
| `live_settings: true` | Its own settings, changed, are sent to it as `settings` (the hello it would start with now) instead of every plugin being restarted. The SDK calls `Plugin::settings` |

An `x-actions` button sends the running plugin `action` (`request`, `action`: the button's `id`,
`value`: the object it's under, as edited, saved or not). It answers with `action.result` (the
same `request`) within 15 seconds, else the button says there was no answer; the answer is shown
only in the browser that pressed it. The SDK's `Plugin::action` gets a `Reply` to answer with,
from any thread. Plugins only run while the recorder records: pressed while it doesn't, the
button says so.

## Testing

The SDK's `testing` module runs your plugin as the recorder would, without the recorder:

```rust
use trunk_recorder_plugin::{testing, HostMessage, Outcome};

let dir = testing::temp_dir("pager");
let hello = testing::hello(&dir, serde_json::json!({ "apiKey": "k" }));
let call = testing::call(&dir, "sys1", 101);
let out = testing::run::<Pager>([HostMessage::Hello(hello), HostMessage::CallConcluded(call)]);
assert!(out.ready());
assert_eq!(out.results()[0].1, Outcome::Ok);
```

| Helper | What it gives you |
|---|---|
| `testing::hello(dir, config)` | A `hello` with your settings and one P25 system, `sys1`, with no settings of its own (set `systems[0].config`). Calls come as WAV and M4A; set `audio_formats` to `["wav"]` to test without an encoder |
| `testing::call(dir, short_name, tg)` | A 3-second call with its files written under `dir`: the JSON, a WAV of silence, and a placeholder M4A |
| `testing::run::<P>(messages)` | Runs the plugin on those messages, ends its input so `shutdown` runs, and returns an `Output`: `ready()`, `results()`, `logs()`, `metrics()`, `status()`, `exit_code` |
| `testing::capture()` | A `Host` whose messages are kept, for testing parts (like a `CallQueue`) on their own |
| `testing::MockServer` | A web server on 127.0.0.1 that answers with your closure and keeps the requests (`form_field`, `form_file_name`, `header`) |
| `testing::temp_dir(name)` | A fresh, empty folder |

Then try it for real:

- `trunk-pro plugin run <your build> <calls or folder>` sends it recorded calls and prints what it
  says (see [Trying a plugin on recorded calls](README.md#trying-a-plugin-on-recorded-calls)).
  `--settings settings.json` gives it settings: `{"config": {…}, "systems": {"<shortName>": {…}}}`.
- In the app, **Add from a file…** on the Plugins page runs your build while recording (see
  [Running a build of your own](README.md#running-a-build-of-your-own)). That's where you see your
  settings form as users will.

The template's [Testing](https://github.com/TrunkRecorder/trunk-plugin-template/blob/main/docs/testing.md)
page has more.

## Releasing

Tag a version (`git tag v0.1.0 && git push --tags`, matching `Cargo.toml`), and the template's
**Release** workflow builds and publishes a GitHub release with:

| File | |
|---|---|
| `<id>-<version>-x86_64-unknown-linux-gnu.tar.gz` | Linux, x86-64 |
| `<id>-<version>-aarch64-unknown-linux-gnu.tar.gz` | Linux, 64-bit ARM (Raspberry Pi with a 64-bit OS) |
| `<id>-<version>-universal-apple-darwin.tar.gz` | macOS, Apple silicon and Intel |
| `<id>-<version>-x86_64-pc-windows-msvc.zip` | Windows |
| `<id>-<version>.manifest.json` | What `--describe` prints |
| `SHA256SUMS` | Checksums of all of the above |

Each file also gets a build provenance attestation from GitHub. Keep the file names: the recorder
finds the archive for its platform by name, and each archive must hold one folder with the
executable, named after the id, in it.

Once that release exists, anyone can install it with **Install from GitHub…** on the Plugins page
or `trunk-pro plugin install https://github.com/you/trunk-plugin-pager`, checked against its
`SHA256SUMS` and marked **Not reviewed**. The recorder refuses a release without `SHA256SUMS` or the
manifest file, and one whose id is already a different plugin's in the registry.

The template's [Releasing](https://github.com/TrunkRecorder/trunk-plugin-template/blob/main/docs/releasing.md)
page covers versions, the checks to run before tagging, and dependencies that make cross-compiling
hard (use a pure-Rust TLS client such as `ureq`).

## Publishing to the registry

The plugin store lists the plugins in the registry,
[github.com/TrunkRecorder/plugins](https://github.com/TrunkRecorder/plugins). Each plugin has an
entry, `plugins/<id>.json`, that pins one release: its version, `api`, tag, the commit the tag
pointed at, and the URL and SHA-256 of each platform's archive. `index.json` gathers every entry,
and is what the recorder fetches. The recorder installs only downloads that match the entry's
checksum, then runs `--describe` and checks that the id and version match too.

To get listed (from the registry's README):

1. Release your plugin with the template's release workflow.
2. Fork the registry and write your entry with its scripts (they need Python 3.9 or later and the
   GitHub CLI, `gh`):

   ```bash
   ./add-release https://github.com/you/trunk-plugin-pager v0.1.0
   ./check plugins/pager.json
   ```

   `add-release` writes `plugins/<id>.json` from the release and rebuilds `index.json`; `check`
   downloads every file and checks it.
3. Open a pull request with both files.

CI on the pull request checks the entry's fields, that every URL is a download from your
repository at the tag, that the tag still points at the commit, every file's SHA-256 and build
provenance (`gh attestation verify`), and that the Linux build's `--describe` agrees with the entry.
A maintainer then reviews the rest: the source is public and licensed for listing, it does what it
says and sends nothing anywhere the user didn't configure, secrets are marked `x-secret` and never
logged, it copes without M4A if it asks for it, it starts with its default settings or says what to
set, and the id is fitting.

Plugins maintained with the recorder are listed as `official`; everyone else's as `community`. An
update is the same steps with the new tag, and keeps its tier. Users see **Update to vX** on the
Plugins page, or run `trunk-pro plugin update`; their settings carry over.

The registry is live, and the recorder's store, installer and `trunk-pro plugin` commands use it
today. If the registry can't be reached, the recorder uses the list it fetched last, or the copy
built into the app at release time, so a newly listed plugin appears in older copies of the app
only once they can reach the registry.

## Plugins in other languages

Nothing requires Rust: any executable that answers `--describe` with a manifest and speaks the JSON
lines protocol on stdin and stdout works. Both sides ignore message types and fields they don't
know. The template's [protocol page](https://github.com/TrunkRecorder/trunk-plugin-template/blob/main/docs/protocol.md)
describes the wire format, and the SDK's `protocol` module is its reference. For the plugin store,
you'd still need releases with the same file names, `SHA256SUMS` and manifest file that the
template's workflow makes.
