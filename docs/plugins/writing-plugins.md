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
| The published plugins ([OpenMHz](https://github.com/TrunkRecorder/trunk-plugin-openmhz), [Broadcastify](https://github.com/TrunkRecorder/trunk-plugin-broadcastify), [Rdio Scanner](https://github.com/TrunkRecorder/trunk-plugin-rdioscanner), [simplestream](https://github.com/TrunkRecorder/trunk-plugin-simplestream), [upload-script](https://github.com/TrunkRecorder/trunk-plugin-upload-script)) | Complete plugins to learn from |

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
   plugin's settings, every system (short name, kind, and the plugin's settings for it), the
   recordings folder, the data folder, and the audio formats calls will carry.
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
| `call.start` | `CallInfo` | A call starts: talkgroup and its tag, frequency, TDMA slot, analog / encrypted / emergency flags, whether it's recorded (and why not), the radios heard so far, patched talkgroups |
| `call.end` | `CallInfo` | A call ends; its files follow in `call.concluded` |
| `call.concluded` | `ConcludedCall` | A recorded call's files are on disk: its path key, its call JSON (Trunk Recorder's format) and the paths of its `.json`, `.wav` and, when asked for, `.m4a` |
| `unit` | `UnitEvent` | A radio registered, deregistered, affiliated, acknowledged, reported its location, got a data grant, or sent an answer request or call alert |
| `audio` | `AudioChunk` | Live audio of recording calls: 16-bit mono PCM at 8 kHz, base64 |
| `status` | `Status` | Every system's state every few seconds: control channel, decode rate, active and recording calls |

Every event carries the system's `short_name`, which is its identity. The numeric `system` index is
for the current run only, so store anything by short name.

### What a plugin sends back

| Message | Meaning |
|---|---|
| `ready` | It read `hello` and is running |
| `log` | A log line at `error`, `warn`, `info` or `debug`. All but debug show in the plugin's log in the interface and in the recorder's log |
| `status` | Its health (`ok`, `warning`, `error`) and a message, shown on its card until the next one |
| `call.result` | What became of a concluded call: `ok`, `skipped` or `failed`, a message, and optionally a URL |
| `metrics` | Figures for the Health tab: queue depth, retries, in flight, bytes sent, latency, the services it talks to (up, degraded, down), and any of its own |

Lines on stdout that aren't protocol messages become log lines, and so does everything on stderr.

**If you subscribe to `call.concluded`, send a `call.result` for every call.** The recorder keeps
each call's files until every plugin taking recorded calls has reported on it, then deletes what
the user's **Files** settings don't keep. For a call your plugin never reports on, the recorder
waits an hour, then gives up and keeps all its files; it's counted as neither done nor failed.

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
trunk-recorder-plugin = "0.2.1"
schemars = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

A plugin implements the `Plugin` trait and calls `run` from `main`:

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
| `Plugin` | The trait. `Config` and `SystemConfig` are your settings types (`NoConfig` for none). `manifest()`, `start()`, one method per topic (`call_start`, `call_end`, `call_concluded`, `unit`, `audio`, `status`) and `shutdown(grace)`. Every method runs on the one thread reading the recorder's messages, so return quickly and hand network or disk work to threads of your own |
| `manifest!()` | A `Manifest` with the id, version, description, homepage, repository, authors and license from `Cargo.toml`; set `name`, `subscribe` and `audio_formats` |
| `run::<P>()` | Answers `--describe` (and `--version`), parses `hello` and your settings, calls your methods, and exits with 78 when `start` returns `Err` |
| `Setup` | What `start` gets: `config`, `systems` (each with `index`, `short_name`, `kind` and your `SystemConfig` or `None`), `capture_dir`, `data_dir`, `audio_formats`; `system_named()` and `has_format()` |
| `Host` | The way back, usable from any thread: `info`/`warn`/`error`/`debug`, `status`, `call_result`, `metrics`. Never `println!` in a plugin: stdout belongs to the protocol |
| `CallQueue`, `QueueOptions`, `Attempt` | An upload queue on worker threads (2 by default) with retries after 10 s, 1 min, 5 min and 15 min, a warning status while calls wait, `call.result`s and `metrics` sent for you, and calls still waiting at shutdown saved to the data folder (`QueueOptions::saved_in`) and resumed next start. Your closure returns `Attempt::Done`, `Skip`, `Retry` or `Fail` |
| `TalkgroupFilter` | Trunk Recorder's talkgroup allow and deny patterns (`507*`, `12?45`); `filter::patterns` deserializes a list that mixes numbers and strings |
| `Multipart` | A `multipart/form-data` body for any HTTP client |
| `testing` | Run a plugin without the recorder (below) |

Four of the five published plugins use `CallQueue` (simplestream, which is live, doesn't), and each
is short enough to read in one sitting.

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

`Option<T>` becomes `T` (empty means not set), unit-variant enums become menus, and nested structs
are inlined. The template's [Settings](https://github.com/TrunkRecorder/trunk-plugin-template/blob/main/docs/settings.md)
page has examples, and advice on changing settings in later versions without breaking existing
installs. Mark API keys and passwords `x-secret`, and never log them.

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
