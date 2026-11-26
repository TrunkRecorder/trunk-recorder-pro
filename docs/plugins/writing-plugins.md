# Writing plugins

This is the guide to building a plugin of your own: starting from the template, how a plugin runs,
the Rust SDK, settings, testing, releasing and publishing to the registry. Every event and message
field by field is in the reference pages it links to.

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
| [trunk-plugin-template](https://github.com/TrunkRecorder/trunk-plugin-template) | A working example plugin with tests, CI and a release workflow |
| [Plugin events](../developer/plugins/events.md) | Every topic a plugin can subscribe to, field by field |
| [Writing an uploader](../developer/plugins/uploading.md) | Keys, queues, retries, requests and node health reports: a plugin that sends calls to a service |
| [Plugin protocol](../developer/plugins/protocol.md) | The wire format and every message, for plugins in other languages, and the settings schema hints |
| [docs.rs/trunk-recorder-plugin](https://docs.rs/trunk-recorder-plugin) | The SDK's API reference |
| [TrunkRecorder/plugins](https://github.com/TrunkRecorder/plugins) | The registry the plugin store installs from |
| The published plugins ([OpenMHz](https://github.com/TrunkRecorder/trunk-plugin-openmhz), [Broadcastify](https://github.com/TrunkRecorder/trunk-plugin-broadcastify), [Rdio Scanner](https://github.com/TrunkRecorder/trunk-plugin-rdioscanner), [simplestream](https://github.com/TrunkRecorder/trunk-plugin-simplestream), [upload-script](https://github.com/TrunkRecorder/trunk-plugin-upload-script), [MQTT](https://github.com/TrunkRecorder/trunk-plugin-mqtt), [Notifications](https://github.com/TrunkRecorder/trunk-plugin-notify)) | Complete plugins to learn from |

## Starting from the template

You need:

- **Rust**, from [rustup.rs](https://rustup.rs). Any recent stable version.
- **Trunk Recorder Pro**, set up to record something. You test against calls it has already
  recorded, so no radio needs to be attached while you work.
- **ffmpeg**, optionally, if your plugin wants M4A audio ([Audio](#audio)).

It takes about fifteen minutes, most of it compiling.

1. On GitHub, press **Use this template** on
   [trunk-plugin-template](https://github.com/TrunkRecorder/trunk-plugin-template), naming the new
   repository after your plugin (`trunk-plugin-pager`, say). Clone it.
2. Pick your plugin's **id**: lowercase letters, digits and dashes, like `pager` or
   `mqtt-status`. It names the executable, the release files, the install folder and the registry
   entry, and it can't change once people have installed your plugin. Then change these:

   | File | Field | Example |
   |---|---|---|
   | `Cargo.toml` | `name`: the id | `pager` |
   | `Cargo.toml` | `description`: one sentence, shown in the plugin store | `Pages your phone when a talkgroup goes active.` |
   | `Cargo.toml` | `repository` | `https://github.com/you/trunk-plugin-pager` |
   | `Cargo.toml` | `authors`, `license` | |
   | `src/main.rs` | `name` in `manifest()`: the display name | `Pager` |
   | `LICENSE` | Your license | |

   Rewrite `README.md` for your plugin's users ([below](#a-readme-for-your-plugin)), and delete
   `examples/settings.json` or rewrite it with your settings.
3. Build it and ask it who it is:

   ```bash
   cargo build
   ./target/debug/pager --describe
   ```

   `--describe` prints the plugin's **manifest**: its id, version, the topics it subscribes to,
   and the schema of its settings. The recorder reads it to decide how to run the plugin and how
   to draw its settings form.
4. Run it against calls you've recorded. `trunk-pro plugin run` starts a plugin the way the
   recorder does, sends it calls from disk as `call.concluded` events, and prints everything it
   says back:

   ```bash
   trunk-pro plugin run ./target/debug/pager ~/TrunkRecorderPro --limit 5
   ```

   ```
   M4A: none
   [pager] status ok: running
   [pager] ✓ sys1/2026/9/30/101-1790771550_857587500
   ```

   To give it settings, put them in a file like the template's `examples/settings.json`
   (`config` is the plugin's settings, `systems` its settings for each system by short name) and
   add `--settings settings.json`. See
   [Trying a plugin on recorded calls](README.md#trying-a-plugin-on-recorded-calls).
5. Replace the example (**call-log**, which writes a line of JSON per call) with your plugin in
   `src/main.rs`. It shows each part a plugin has: `Config` and `SystemConfig` (the settings, with
   doc comments that become the form's labels), `manifest()`, `start()` (checks the settings and
   sets up; an `Err` shows the user what's wrong), one method per topic you subscribed to, and
   `shutdown()`. Read [How a plugin runs](#how-a-plugin-runs) before you do anything slow, such as
   network calls, in one of those methods. Keep `cargo test` passing, and run `cargo fmt`: the
   template's `rustfmt.toml` sets the recorder's style, and its CI checks it.
6. Run it inside your recorder: **Add from a file…** on the Plugins page
   ([Running a build of your own](README.md#running-a-build-of-your-own)). Press **Set up** to see
   your settings form on its card in Setup → Plugins, as users will; turn it on and its
   `SystemConfig` form shows on each system's card. Start recording, and its card on the Plugins
   page shows whether it's running, the calls it handled, its status and its recent log. After you
   rebuild, **Reinstall** runs the new build.
7. [Release it](#releasing) and [submit it to the registry](#publishing-to-the-registry).

### A README for your plugin

Users read it on GitHub before they install, and reviewers read it before listing your plugin.
Cover what it needs, its settings, and what it sends where:

```markdown
# Pager

Pages your phone when a talkgroup goes active. A plugin for Trunk Recorder Pro.

## What it needs
An ntfy.sh topic (free). Nothing to install.

## Settings
| Setting | |
|---|---|
| Topic | Your ntfy.sh topic. |
| Talkgroups | The talkgroups to page for, by number. |

## What it sends where
A notification with the talkgroup's name to ntfy.sh, for each call on your talkgroups. Nothing else.
```

"What it sends where" matters: users are trusting your plugin with their recorder, and the
registry's review checks it.

## How a plugin runs

The recorder runs each plugin that's on as its own process. Because of that:

- **A plugin can't crash the recorder.** If it dies, the recorder logs it and starts it again.
  Recording goes on.
- **A plugin can't slow the recorder down.** Events are queued for each plugin. One that falls far
  behind loses events; the recorder never waits for it.
- **Plugins only watch.** Nothing a plugin says changes what gets recorded, which radios are
  followed, or anything else about the recorder.
- **Plugins can be written in any language.** The SDK handles the protocol for Rust;
  [Plugin protocol](../developer/plugins/protocol.md) has the wire format for the rest.

The lifecycle, as the recorder (the "host") sees it:

```
recorder                                   plugin
────────                                   ──────
run `plugin --describe`  ───────────────►  prints its manifest, exits
start `plugin` (no arguments)
hello  ─────────────────────────────────►  Plugin::start(host, setup)
       ◄─────────────────────────────────  ready          (or: status error, exit 78)
call.concluded  ────────────────────────►  Plugin::call_concluded(call)
       ◄─────────────────────────────────  call.result, log, status …
…
shutdown  ──────────────────────────────►  Plugin::shutdown(grace)
close stdin                                exits
(kill, if it's still running after the grace period)
```

1. **Describe.** When a plugin is installed, added, or about to start, the recorder runs it with
   `--describe` and reads its [manifest](#the-manifest). It refuses a manifest with no `api`, or
   an `api` newer than its own (`API_VERSION` is 1).
2. **Start.** When recording starts, the recorder runs the executable with no arguments, in its
   data folder (`plugin-data/<id>/`), and sends `hello`: the plugin's settings, every system it's
   on for (each with its permanent id, its sites and the plugin's settings for it), the
   recordings folder, the data folder, and the audio formats calls will carry. The SDK parses the
   settings into your types and calls `Plugin::start`.
3. **Ready.** If `start` returns `Ok`, the SDK sends `ready`. If it returns `Err`, the SDK sends an
   error `status` with it and exits with status **78**: the recorder shows the message and doesn't
   restart the plugin until its settings change or recording starts again.
4. **Events.** The recorder sends the events the plugin subscribed to, and nothing else. The SDK
   calls your method for each, one at a time, in order.
5. **Shutdown.** When recording stops, or the plugin's settings change and it doesn't take them as
   it runs, the recorder sends `shutdown` with a grace period (10 seconds when recording stops),
   then closes stdin. The SDK calls `Plugin::shutdown(grace)`, and the process exits when it
   returns. One still running after the grace period is killed.

Plugins run only while the recorder records.

### The manifest

`Plugin::manifest()` returns it. Start from `trunk_recorder_plugin::manifest!()`, which fills in
the id, version, description, homepage, repository, authors and license from `Cargo.toml`, and set
what's yours:

```rust
fn manifest() -> Manifest {
    Manifest {
        name: "Pager".into(),
        subscribe: vec![topic::CALL_START.into()],
        ..trunk_recorder_plugin::manifest!()
    }
}
```

| Field | What for |
|---|---|
| `name` | The display name |
| `subscribe` | The [topics](#topics) to receive |
| `audio_formats` | `["m4a"]` for M4A with recorded calls ([Audio](#audio)) |
| `system_overrides`, `site_overrides` | The settings a system may set again for itself, and a site ([Settings](#settings-a-system-overrides)) |
| `page`, `contribute`, `live_settings` | A page of its own, buttons elsewhere in the interface, new settings taken while it runs ([More room, buttons and live settings](#more-room-buttons-and-live-settings)) |

The SDK fills in `api` and the settings schemas (`config`, `system_config`) from your types. Every
field is in [Plugin protocol](../developer/plugins/protocol.md#--describe).

### Topics

A plugin subscribes to the topics it uses, and is sent nothing else. The recorder doesn't even
build events nobody subscribed to, so subscribe only to what you use.

| To | Subscribe to |
|---|---|
| Upload or archive recorded calls | [`call.concluded`](../developer/plugins/events.md#callconcluded): its files are on disk |
| Follow calls as they happen, recorded or not | [`call.start`, `call.update`, `call.end`](../developer/plugins/events.md#callstart-callupdate-and-callend) |
| Stream live audio | [`audio`](../developer/plugins/events.md#audio) (with `call.start` and `call.end`) |
| Relay digital voice without decoding it | [`voice`](../developer/plugins/events.md#voice): the vocoder frames |
| Decode something the recorder doesn't (pagers, data), or watch how channels decode | A signal tap, [`tap.<id>`](../developer/plugins/events.md#tapid): the FM discriminator, voice frames off the air, frame errors. Subscribing turns it on |
| Watch radios register, affiliate, call for help | [`unit`](../developer/plugins/events.md#unit) |
| Act on pages, alert tones, DTMF and Morse IDs | [`tone_pattern`](../developer/plugins/events.md#tone_pattern) |
| Show how each site is doing | [`status`](../developer/plugins/events.md#status), every 5 s |
| Measure, or send node health reports | [`totals`](../developer/plugins/events.md#totals), every 10 s |
| Pass the recorder's log on | [`log`](../developer/plugins/events.md#log) |

Every event carries its system's `system_id`, its identity across runs and renames: store anything
by it, not by `short_name` or the numeric `system`. How events name systems and sites, and what a
call heard on several sites looks like, is in
[Systems and sites in events](../developer/plugins/events.md#systems-and-sites-in-events).

### One thread: return quickly

Your event methods run on the thread that reads stdin, one event at a time. While a method runs,
the next events wait in the plugin's queue, which holds 1024; a plugin that falls that far behind
loses events ([Order and loss](../developer/plugins/events.md#order-and-loss)). So an event method
should return in milliseconds. Hand anything slower to another thread: network requests,
spawning programs, big file copies.

- **For recorded calls**, use the SDK's `CallQueue`. It runs your function on background threads,
  retries with backoff, reports results, and saves what's left at shutdown. See
  [Writing an uploader](../developer/plugins/uploading.md).
- **For other events** (live audio to a stream, unit events to a database), start a thread in
  `start` and send it work over a channel (`std::sync::mpsc`). Join it in `shutdown`.

The template's example writes a line to a file inline, which is fast enough.

### Talking back

`Plugin::start` gets a `Host`. Keep it: it's how the plugin reports back. It's cheap to clone and
works from any thread.

| Call | Message | Shown |
|---|---|---|
| `host.info(…)`, `warn`, `error`, `debug` | `log` | In the plugin's log in the interface and in the recorder's log, prefixed with its id. `debug` lines are dropped |
| `host.status(State::Warning, "OpenMHz is down; 12 calls queued")` | `status` | On its card, until the next status: `Ok`, `Warning` or `Error` |
| `host.notice("token", State::Warning, "The API key was refused")`, `host.clear_notice("token")` | `notice` | What stays true while it runs (a setting to fix), under the status: a later `status` doesn't hide it. One per key (since SDK 0.16.0) |
| `host.call_result(path, Outcome::Ok, "", url)` | `call.result` | What became of a recorded call: `Ok`, `Skipped` (deliberately not handled) or `Failed`, a message and optionally a URL. `call_result_took` also says how long your work on it took ([below](#what-becomes-of-a-call)) |
| `host.metrics(&Metrics { … })` | `metrics` | On the Health tab: queue depth, retries, in flight, bytes sent, latency, the services it talks to (up, degraded, down), and any figures of its own. `CallQueue` sends these for you |
| `host.calls(CallsQuery { … })` | `calls.query` | Recorded calls, asked of the recorder ([below](#recorded-calls)) |
| `host.save_settings(&value)` | `settings.save` | Some of its own settings, saved ([below](#saving-its-own-settings)) |
| `reply.ok("Sent")`, `reply.fail(why)` | `action.result` | The answer to one of its buttons ([below](#more-room-buttons-and-live-settings)) |

**Never print to stdout.** A plugin's stdout carries the protocol, so `println!` sends the
recorder something that isn't a message (it logs such a line rather than choke on it). Use
`host.info()`, or `eprintln!` for raw output: stderr goes to the plugin's log.

### What becomes of a call

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

### Recorded calls

A plugin that needs calls from before (a talkgroup's last week, a radio's history) asks the
recorder for them. In the Rust SDK:

```rust
let request = host.calls(CallsQuery { talkgroup: Some(101), limit: Some(50), ..Default::default() });
```

Any of `system` (a short name), `talkgroup`, `unit` (calls that radio talked on), `from` and `to`
(Unix ms) and `limit` (1 to 500; 100 when left out) narrow the calls. The page arrives at
`Plugin::calls(request, page)` with the number `host.calls` returned: `page.entries` (each call's
`path`, its `record`, and whether its audio and `.json` are still kept, newest first), `page.next`
when more remain (put it in the next query's `cursor` for the next page), and `page.error` when the
recorder's call store is off and the page is only the recent calls. It's the recorder's call
store, the latest calls included. Ask this way rather than opening `trunk-pro.sqlite`: its tables
are the recorder's own and change between versions. (Since SDK 0.9.0.)

### Saving its own settings

A plugin can fill in what it learns that a user would otherwise type (a name for an ID they gave,
say), or record a change made through it elsewhere (a chat command that adds a destination), with
`host.save_settings(&value)` (your `Config` whole is fine). The recorder writes the settings to
its config as an edit of its own: the form shows them at once, in every browser, and they come
back to the plugin as new settings. The answer then arrives at `Plugin::settings_saved(request,
ok, message)`.

A save is refused when it would restart the plugin: it has to take its settings as it runs
(`live_settings`), run as one copy, have no per-system settings, and keep every setting it needs
set; the `message` says which. Each save that changes something is logged. Mark the fields it
fills in `readOnly` so the form shows them rather than letting them be typed over. This is for
settings, which the user sees and edits; a plugin's own state goes in its data folder. (Since SDK
0.10.0.)

### When things go wrong

| What happens | What the recorder does |
|---|---|
| `start` returns `Err` (exit status 78) | Shows the error. Doesn't restart the plugin until its settings change or recording next starts |
| The plugin panics or exits | Logs it and restarts it after 1 s, then 2 s, 4 s, … up to a minute between tries; back to 1 s once it has run for a minute. Events wait in its queue meanwhile |
| The plugin falls behind | Drops its events while its queue is full (but not calls' starts and ends), and logs how many |
| `--describe` fails or prints nonsense | Doesn't start the plugin, and says why |
| It's still running after the shutdown grace period | Kills it |

### Files and trust

- **Data folder** (`setup.data_dir`): the plugin's own, kept across restarts and upgrades. Put
  queues, state and caches here. The process starts with it as its working directory.
- **Capture folder** (`setup.capture_dir`): where the recorder keeps calls, always a full path, as
  are the paths in each call's `files`. Read the files events point to, but don't move, change or
  delete them: other plugins, and the recorder's call history, use them too.

A plugin runs as the same user as the recorder, with the same access to files and the network.
Plugins in the registry are reviewed before they're listed, and pinned to the exact builds that
were reviewed. Users choose what they install.

## The Rust SDK

Add the crate, and `schemars` for the settings form:

```toml
[dependencies]
trunk-recorder-plugin = "0.17.0"
schemars = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

Use the latest version on [crates.io](https://crates.io/crates/trunk-recorder-plugin) (0.17.0 when
this was written). The recorder's
[CHANGELOG](https://github.com/TrunkRecorder/trunk-recorder-pro/blob/main/CHANGELOG.md) notes each SDK release beside
the recorder's, and says what a plugin must change. Both sides ignore what they don't know, so a
plugin built on an older SDK keeps running on a newer recorder while `API_VERSION` stays 1; rebuild
for the newer topics and fields. A plugin built on an SDK older than 0.5.0 must be rebuilt (systems
are numbered from 0, conventional ones too, and the call record names every field instead of
keeping some in `extra`), and so must one older than 0.7.0 that takes tone patterns (the topic is
`tone_pattern` now, with `text`, and KNOX digits are `knox: true`).

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
        self.host.info(format!("TG {} for {} ms", call.call.talkgroup, call.call.audio_ms()));
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
| `Plugin` | The trait. `Config` and `SystemConfig` are your settings types (`NoConfig` for none). `manifest()`, `start()`, one method per topic (`call_start`, `call_update`, `call_end`, `call_concluded`, `unit`, `audio`, `voice`, `tap`, `status`, `tone_pattern`, `log_line`, `totals`), `action(a, reply)`, `settings(setup)`, `calls(request, page)`, `settings_saved(request, ok, message)` and `shutdown(grace)`. Every method runs on the one thread reading the recorder's messages |
| `manifest!()` | A `Manifest` with the id, version, description, homepage, repository, authors and license from `Cargo.toml` |
| `run::<P>()` | Answers `--describe` (and `--version`), parses `hello` and your settings, calls your methods, and exits with 78 when `start` returns `Err` |
| `Setup` | What `start` gets: `config`, `systems` (each a `System`: `index`, `id`, `short_name`, `kind`, `name`, its FCC licenses (`fcc`, `fcc_call_sign`, `fcc_name`), `sites`, `config` (your `SystemConfig` or `None`) and `settings` (your `Config` as it is for the system)), `host` (the recorder's `name` and `version`), `capture_dir`, `data_dir`, `audio_formats`; `system_named()` and `has_format()`. The name and FCC licenses are optional, so often empty |
| `Host` | The way back, usable from any thread ([Talking back](#talking-back)) |
| `Reply` | The answer to a button: `ok(message)` or `fail(message)`, once, from any thread; dropped unsent, it answers that the plugin didn't |
| `CallQueue`, `QueueOptions`, `Attempt` | An upload queue on worker threads, with retries, results and metrics sent for you, and calls still waiting at shutdown saved for the next start ([Writing an uploader](../developer/plugins/uploading.md)) |
| `http::Failure` | What an HTTP status means for something sent: try again, wait, or give up |
| `Talkgroups`, `TalkgroupFilter` | A system's talkgroups for your `SystemConfig` ([Choosing talkgroups](#choosing-talkgroups)), and allow and deny lists of numbers, ranges and patterns |
| `Multipart` | A `multipart/form-data` body for any HTTP client |
| `health` | Node health reports made of `totals` ([Node health reports](../developer/plugins/uploading.md#node-health-reports)) |
| `testing` | Run a plugin without the recorder ([Testing](#testing)) |

Four of the published plugins use `CallQueue` (OpenMHz, Broadcastify, Rdio Scanner and Upload
script; simplestream, which is live, and MQTT, which has a queue of its own, don't), and the
smaller ones are short enough to read in one sitting.

## Settings

A plugin describes its settings as Rust structs. The recorder draws a form from them, stores what
the user enters, and hands it back to the plugin at startup. You never write form code.

- **`Config`**: settings for the plugin as a whole, like a server URL. Drawn on its card in Setup
  → Plugins.
- **`SystemConfig`**: settings the form repeats on each system's card, like an API key per system.
  Many services give each system its own key, so uploaders need this.

Use `NoConfig` for either if you don't need it.

```rust
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
struct Config {
    /// Server
    ///
    /// Where calls are uploaded. Leave it as it is unless you run your own.
    #[schemars(url)]
    server: String,
    /// Upload encrypted calls
    upload_encrypted: bool,
    /// Retries
    ///
    /// How many times to try a call before giving up.
    #[schemars(range(min = 0, max = 10))]
    retries: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config { server: "https://api.example.com".into(), upload_encrypted: false, retries: 3 }
    }
}
```

- **`Deserialize`** reads what the user entered; **`JsonSchema`** (from
  [`schemars`](https://docs.rs/schemars/1), version 1) describes the struct to the form.
- **`Serialize`** lets the schema include your defaults, so the form starts filled in with them.
  Without it, fields start empty.
- **`#[serde(default)]`** lets the user leave any field empty: it then takes its value from
  `Default`. Leave it out and every field becomes required, and existing installs break the day
  you add a field.
- **`#[serde(rename_all = "camelCase")]`** isn't required, but it's the recorder's convention.

Doc comments become the form's text: the first paragraph is the field's **label**, the rest the
**help** shown under it (line breaks inside a paragraph are joined up). Keep labels short, and
write help for someone who isn't a programmer. The doc comment on the struct itself isn't shown.
Fields appear in the order you declare them.

| Rust | Form |
|---|---|
| `String` | A text box |
| `String` with `#[schemars(extend("x-secret" = true))]` | A password box. Mark API keys and passwords this way, and never log them |
| `String` with `#[schemars(url)]` | A text box that checks for a URL |
| `String` with `#[schemars(extend("x-multiline" = true))]` | A text area |
| `bool` | A switch |
| `u32`, `i64`, `f64`, … with `#[schemars(range(min = …, max = …))]` | A number box |
| An enum of unit variants | A menu; a variant's doc comment is its label |
| `Vec<String>`, `Vec<u32>`, … | A list |
| `Option<T>` | The same as `T`; empty means `None` |
| A struct | A group of its fields (nested structs are inlined) |
| `Vec` of a struct | Groups, added and removed one by one. The struct's doc comment names each ("Stream 1", "Add stream") |
| `String` with `#[schemars(extend("x-system" = true))]` | A menu of the recorder's systems, by short name; follows renames |

More hints for bigger forms (systems or signals to tick, talkgroups typed in and checked, a list
naming another list's items, typed-in tones, list items titled and summed up, fields shown only
when they apply, buttons, sliders, fields the plugin fills in itself) are in
[Settings schemas](../developer/plugins/protocol.md#settings-schemas). They go on a field with
`#[schemars(extend("x-ref" = "destinations"))]`, or on a struct used as an array's items with the
same attribute on the struct. The form doesn't handle maps or enums that carry data: keep settings
flat, and name things users know ("API key", not "auth token").

A menu:

```rust
#[derive(Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
enum Quality {
    /// Low (smaller files)
    Low,
    #[default]
    /// Normal
    Normal,
}
```

### Fields that have to be filled in

Mark a field the plugin can't do without with `x-required`:

```rust
/// API key
#[schemars(extend("x-secret" = true, "x-required" = true))]
api_key: String,
```

Until it's filled in, a required field in `Config` marks the plugin **Needs setting up**, and one
in `SystemConfig` marks that system **Not set up** for your plugin, on the system's card and in
the row of systems on your plugin's card. Use it for what makes a system count as set up, such as
an upload key, even when an empty key just means "don't upload this system".
`#[schemars(required)]` does nothing on a `#[serde(default)]` struct, which is why it's marked this
way; the SDK turns these marks into the schema's `required` list. Marking a field doesn't stop the
user saving without it, so still check in `start`.

### Settings for each system

In `start`, `setup.systems` lists every system your plugin is on for, each with its `id`, short
name, `kind` (`p25`, `smartnet`, `dmr`, `nxdn`, `edacs` or `conventional`), its `sites`, and your
`SystemConfig` (`None` when the user left that system's settings empty). A system's `name` and FCC
licenses are there when known, which often they aren't.

Build your lookups by **`id`**: the same across runs and renames, and every event carries it
(`system_id`; a concluded call's `call.call.system_id`):

```rust
let keys: HashMap<u32, String> = setup
    .systems
    .iter()
    .filter_map(|s| Some((s.id, s.config.as_ref()?.api_key.clone())))
    .filter(|(_, k)| !k.is_empty())
    .collect();
```

The **short name** is what to show in logs and messages; `setup.system_named(name)` finds a system
by it. The **`index`** is the number events carry as `system`, good for this run only, so don't key
anything you save on it. Each conventional system is a system of its own; tell one by its `kind`,
not by its number.

When `SystemConfig` has a field with the same name as one in `Config`, the system's field shows the
`Config` value as its placeholder: a setting for every system that each can override. Your plugin
decides what an empty system field means; the upload-script plugin uses its main script.

### Settings a system overrides

You can let a system set some of your recorder-wide settings (`Config`) again for itself, for
example to send its calls to another server. Name those settings (their keys, as the form has them)
in the manifest's `system_overrides`:

```rust
fn manifest() -> Manifest {
    Manifest { system_overrides: vec!["server".into()], ..trunk_recorder_plugin::manifest!() }
}
```

A system's card in Setup then offers those settings, and no others. None is the default: every
system has your settings as they are. Name only the ones you read per system; the recorder ignores
any other a system has saved (with a warning in your log, and **Remove** on your card in Setup).

Read those for a system from `System::settings`, which is your `Config` with what that system set
again, and the rest from `Setup::config`. An uploader that builds one client per server and starts
its `CallQueue` with `start_by`, keyed by server, handles it all
([One queue for each server](../developer/plugins/uploading.md#one-queue-for-each-server)). (Since
SDK 0.13.0.)

### Settings for each site

A system with several sites can give each of them some of your `SystemConfig` of its own: those
you name in the manifest's `site_overrides`, such as the account an uploader sends that site's
calls to. A system's card in Setup then has a row for each of its sites with those fields, empty
ones taking the system's. Each `System::sites` entry has the site's `id`, `name` and `config`: your
`SystemConfig` with what the site set again (`None`: as the system's).

Send a recorded call with `System::targets(&call.call)`: the settings of each site that heard it
that has its own, and the system's once when one that heard it has none (or it names no site). A
call two such sites heard goes to both. (Since SDK 0.14.0.)

```rust
fn manifest() -> Manifest {
    Manifest { site_overrides: vec!["apiKey".into(), "systemId".into()], ..trunk_recorder_plugin::manifest!() }
}
// …
for t in system.targets(&call.call) {
    upload(t.config, &call); // t.site: the site whose own these are (None: the system's)
}
```

### Choosing talkgroups

For a plugin that lets each system pick the talkgroups it takes, flatten the SDK's `Talkgroups`
into your `SystemConfig`. It's the **Talkgroups** menu (every talkgroup, only some, all but some)
and its lists, as the uploaders have it:

```rust
use trunk_recorder_plugin::Talkgroups;

#[derive(Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase", default)]
struct SystemConfig {
    /// API key
    #[schemars(extend("x-secret" = true))]
    api_key: String,
    #[serde(flatten)]
    talkgroups: Talkgroups,
}
```

In `start`, `c.talkgroups.filter_for(&s.short_name)?` checks every entry and gives a
`TalkgroupFilter`; `filter.passes(call.call.talkgroup)` then says whether a call is one of them. An
entry that isn't a talkgroup, or **Only some** with none listed, is an error that names the system.
(Since SDK 0.8.0.)

### Systems and sites it's off for

The user can turn your plugin off for a system, or for one of a system's sites. A system it's off
for isn't in `setup.systems`, and nothing about it is sent; a site it's off for isn't among its
system's `sites`. There's nothing to do in the plugin; just don't count on hearing of every system
or site. What is and isn't sent is in
[Systems and sites a plugin is off for](../developer/plugins/events.md#systems-and-sites-a-plugin-is-off-for).

### Checking settings

The SDK rejects settings that don't parse, such as text where a number goes, before `start` runs.
Check everything else in `start`, and return an `Err` that tells the user what to fix:

```rust
if keys.is_empty() {
    return Err("Add an API key for at least one system".into());
}
```

The recorder shows the error, and doesn't start the plugin again until its settings change or
recording next starts.

### Where settings live, and changing them later

The settings are in the recorder's `config.json`: the plugin's switch and `Config` under `plugins`,
its `SystemConfig` inside each system. The keys are in the
[plugins configuration reference](../configuration/plugins.md). Empty fields aren't saved, so your
defaults apply. A system's settings go with it when it's renamed or removed; uninstalling a plugin
removes its settings everywhere.

Users upgrade plugins but keep their settings, so:

- **Adding a field** is safe, as long as the struct has `#[serde(default)]`.
- **Renaming a field** loses what users entered. Use `#[serde(alias = "old")]` to keep reading the
  old name. Aliases also let people coming from Trunk Recorder paste their old plugin settings:
  the OpenMHz plugin reads `uploadServer` and `openmhzSystemId`, Trunk Recorder's names, as well
  as its own.
- **Removing a field** is safe, since unknown fields are ignored.
- **Changing a field's type** breaks existing settings. Add a new field instead.

`--describe` prints the schemas the form is drawn from.

### More room, buttons and live settings

A plugin whose settings are more than a few fields (rules and where they send, say) can ask for
more of the interface in its manifest:

| Manifest | What it gets |
|---|---|
| `page: Some(Page { title })` | A page of its own, in the navigation under the radio pages (`#/plugin/<id>`): its settings full width, its state and its log. Its card in Setup links to it |
| `contribute: vec![Contribution { on, label, hint, append, item }]` | Buttons elsewhere in the interface. `on: "signal"` puts one on each of a system's signals in Setup; pressed, it adds `item` to the settings array `append` (with `{system}`, `{signal}`, `{id}` and `{name}` filled in) and opens the plugin's page |
| `live_settings: true` | Its own settings, changed, are sent to it instead of every plugin being restarted. The SDK calls `Plugin::settings(setup)`; returning `Err` keeps the ones it had and shows why |

An `x-actions` button sends the running plugin `action`: the button's `id` and the object it's
under, as edited, saved or not. The SDK's `Plugin::action(a, reply)` gets a `Reply` to answer with,
now or later from another thread, within 15 seconds; else the button says there was no answer. The
answer is shown only in the browser that pressed it. Plugins only run while the recorder records:
pressed while it doesn't, the button says so.

## Audio

`call.concluded` always carries a WAV path (16-bit mono, 8 kHz, lossless). Many services want
**M4A** instead (AAC in an MP4 file, about a tenth of the size): OpenMHz and Broadcastify Calls
both do. List it in the manifest:

```rust
Manifest {
    subscribe: vec![topic::CALL_CONCLUDED.into()],
    audio_formats: vec![format::M4A.into()],
    ..trunk_recorder_plugin::manifest!()
}
```

The recorder then encodes each call once, for every plugin that asked, and puts the file next to
the WAV (`files.m4a`). Encoding happens off the recorder's main thread, so M4A calls reach you a
moment later than WAV-only calls would. When a call comes with an M4A, its WAV may not have been
written: the recorder then writes it only when the user keeps WAVs or a plugin asks for it too. Use
the M4A. A plugin that needs both lists both: `vec![format::M4A.into(), format::WAV.into()]`.

**M4A might not be there.** The recorder doesn't include an AAC encoder; it uses ffmpeg,
afconvert (part of macOS) or fdkaac when one is on the computer, and the user can turn M4A off.
The encoders and their settings are in [M4A files](../guides/recordings.md#m4a-files). You find out
in two places:

- At startup: `setup.has_format(format::M4A)` is `false`. This is where to tell the user, once.
- For each call: `call.files.m4a` is `None`. Encoding one call can fail even when an encoder is
  there.

What to do without M4A depends on the service:

- **It takes WAV too**: upload the WAV, and warn once at startup that uploads are bigger than they
  need to be:

  ```rust
  if !setup.has_format(format::M4A) {
      host.warn("No M4A encoder: uploading WAV (install ffmpeg for smaller uploads)");
  }
  ```

- **It takes only M4A**, like OpenMHz: refuse to start, with an `Err` from `start` that says what to
  install ("OpenMHz needs calls as M4A, and there's no M4A encoder on this computer. Install
  ffmpeg, then start recording again."). The user sees it next to the plugin, which is better than
  failing every call one log line at a time.

Either way, a call can still arrive without `files.m4a` when encoding just that call failed.
Report it as `Failed` ("this call couldn't be encoded as M4A") rather than sending the wrong
format.

The WAV and M4A files belong to the recorder: its call history plays them, and other plugins read
them. To change the audio (normalize it, trim it), write your own copy in your data folder or a
temporary file.

Plugins that stream live audio subscribe to [`audio`](../developer/plugins/events.md#audio)
instead: raw PCM, which the plugin encodes for its stream itself.
[`voice`](../developer/plugins/events.md#voice) has the same digital voice as the vocoder frames it
was decoded from. Neither goes to a plugin the user delays calls for.

## Testing

There are three ways to exercise a plugin, from fastest to most real:

1. **Unit tests** drive the plugin in-process with made-up events.
2. **`trunk-pro plugin run`** runs the built plugin against calls you've recorded.
3. **Inside the recorder**, added with **Add from a file…**.

### Unit tests

The SDK's `testing` module runs your plugin as the recorder would, without the recorder:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use trunk_recorder_plugin::{testing, HostMessage, Outcome};

    #[test]
    fn logs_a_call() {
        let dir = testing::temp_dir("pager");
        let mut hello = testing::hello(&dir, json!({ "file": "out.jsonl" }));
        hello.systems[0].config = json!({ "label": "County" });
        let call = testing::call(&dir, "sys1", 101);

        let out = testing::run::<Pager>([HostMessage::Hello(hello), HostMessage::CallConcluded(call)]);

        assert!(out.ready());
        assert_eq!(out.results()[0].1, Outcome::Ok);
    }
}
```

| Helper | What it gives you |
|---|---|
| `testing::temp_dir(name)` | A fresh, empty folder |
| `testing::hello(dir, config)` | A `hello` with your settings as JSON, and one P25 system, `sys1` (index 0, id 0, no sites). Its settings for that system are `hello.systems[0].config`; push another `SystemInfo` to `hello.systems` for more. The data folder is `dir/data`. Calls come as WAV and M4A; set `hello.audio_formats = vec!["wav".into()]` for a recorder with no M4A encoder |
| `testing::call(dir, short_name, tg)` | A 3-second call (`audio_ms()` 3000) on system 0, with its files written under `dir`: the JSON, a silent WAV, and a placeholder M4A in `files.m4a` (not real audio). Set `files.m4a = None` for a call that couldn't be encoded; `system` and `call.system_id` for another system; `call.sites` for the sites that heard it |
| `testing::run::<P>(messages)` | Runs the plugin on the messages, then ends its input, so `shutdown` runs, and returns an `Output` |
| `out.ready()` | It started: its settings were good |
| `out.exit_code` | `EXIT_CONFIG` (78) if `start` refused the settings |
| `out.results()`, `out.logs()`, `out.status()`, `out.metrics()`, `out.notices()` | What it reported (`notices()`: those still standing, by key). `out.messages` has every message it sent |
| `testing::capture()` | A `Host` that keeps what it's sent, for testing a part of your plugin (like a `CallQueue`) on its own. `captured.output()` reads it back |
| `testing::MockServer` | A web server on 127.0.0.1 ([below](#testing-against-a-server)) |

Other events are structs with defaults, so build the fields you need:
`HostMessage::CallStart(CallInfo { id: 7, talkgroup: 101, ..Default::default() })`.

Things worth a test:

- Good settings start the plugin; each kind of bad setting doesn't, with a message that says what
  to fix.
- A call is handled; calls you skip on purpose are reported as `Skipped`.
- No M4A (`files.m4a = None`) is handled the way you decided.
- A failure (a server that's down, say) is reported as `Failed`, not lost.
- Calls in flight are finished or saved at shutdown.

### Testing against a server

Keep tests off the internet. `testing::MockServer` is a web server on `127.0.0.1` that answers with
a function of yours and keeps every request:

```rust
use trunk_recorder_plugin::testing::MockServer;

let server = MockServer::start(|req| {
    let key = String::from_utf8(req.form_field("api_key").unwrap_or_default()).unwrap();
    if key == "good" { (200, String::new()) } else { (500, "API Keys do not match!\n".into()) }
});
let hello = testing::hello(&dir, json!({ "server": server.url() }));
// … run the plugin …
let req = &server.requests()[0];
assert_eq!(req.path, "/sys1/upload");
assert_eq!(req.form_field("talkgroup_num").unwrap(), b"101");
assert!(req.form_file_name("call").unwrap().ends_with(".m4a"));
```

A `Request` has `method`, `path`, `headers`, `body`, `header(name)`, and `form_field` /
`form_file_name` for `multipart/form-data` bodies. Answer the way the real service does, errors
included, and copy its error messages exactly, because your plugin tells them apart. For a server
that's down, point the plugin at a port nothing listens on, like `http://127.0.0.1:9`. The
[OpenMHz plugin's tests](https://github.com/TrunkRecorder/trunk-plugin-openmhz/blob/main/src/main.rs)
are a full set.

### Against recorded calls, and in the recorder

`trunk-pro plugin run <your build> <calls or folder>` sends your build recorded calls and prints
what it says: `✓` done, `–` skipped, `✗` failed. `--settings settings.json` gives it settings
(`{"config": {…}, "systems": {"<shortName>": {…}}}`), and `--encoder none` tests it without M4A.
It runs with a scratch data folder, so a test run doesn't leave work for the installed plugin. See
[Trying a plugin on recorded calls](README.md#trying-a-plugin-on-recorded-calls), and every option
in the [command-line reference](../command-line.md). To try a real service without touching the
real one, run a stand-in for it on your computer and point your server setting at it.

When `plugin run` is happy, run the plugin inside the recorder for a while
([Running a build of your own](README.md#running-a-build-of-your-own)): that's where you see your
settings form as users will, and the only place the timing is real.

The protocol is JSON lines, so you can also type at a plugin. It answers `{"type":"ready"}` if it
started:

```sh
./target/debug/pager
{"type":"hello","api":1,"config":{},"systems":[{"index":0,"id":1,"short_name":"sys1","kind":"p25"}]}
```

### Against an unreleased SDK

To try changes to the SDK itself, point Cargo at a local checkout of the recorder's repository.
Create `.cargo/config.toml` (the template's `.gitignore` ignores it):

```toml
[patch.crates-io]
trunk-recorder-plugin = { path = "../trunk-recorder-pro/crates/trunk-recorder-plugin" }
```

Delete the file, then run `cargo update -p trunk-recorder-plugin`, before committing `Cargo.lock`.
Otherwise the lock file records your local path, and the release build fails.

## Releasing

Use [semver](https://semver.org) in `Cargo.toml`: a patch (0.1.0 → 0.1.1) for fixes, a minor
version (0.1.1 → 0.2.0) for new features and settings, a major one (0.2.0 → 1.0.0) for changes
that break existing settings or need users to do something. Keep a `CHANGELOG.md` with a
`## [0.1.1]` heading for each version: the release workflow uses that section as the release
notes.

Before you tag:

- `cargo fmt --check`, `cargo clippy -- -D warnings` and `cargo test` pass, and CI is green.
- `--describe` shows the right name, description and repository. The release workflow refuses the
  template's placeholders.
- You've run the release build against real calls with `trunk-pro plugin run`.
- New settings have defaults, so existing installs keep working
  ([Changing them later](#where-settings-live-and-changing-them-later)).

Then set the version in `Cargo.toml`, run `cargo build` so `Cargo.lock` follows, commit both, and
tag the commit with the version (`git tag v0.1.1 && git push origin v0.1.1`). The template's
**Release** workflow checks that the tag matches `Cargo.toml`, then builds and publishes a GitHub
release with:

| File | |
|---|---|
| `<id>-<version>-x86_64-unknown-linux-gnu.tar.gz` | Linux, x86-64 |
| `<id>-<version>-aarch64-unknown-linux-gnu.tar.gz` | Linux, 64-bit ARM (Raspberry Pi 3/4/5 with a 64-bit OS) |
| `<id>-<version>-universal-apple-darwin.tar.gz` | macOS, Apple silicon and Intel |
| `<id>-<version>-x86_64-pc-windows-msvc.zip` | Windows |
| `<id>-<version>.manifest.json` | What `--describe` prints |
| `SHA256SUMS` | Checksums of all of the above |

Each archive holds one folder, `<id>-<version>-<target>/`, with the executable (named after the
id), `README.md` and `LICENSE`.
These are the platforms Trunk Recorder Pro ships for; the Linux builds link against glibc 2.28, so
they run on any distribution from about 2019 on. **Keep the file names**: the recorder finds the
archive for its platform by name, and the registry pins them by checksum.

Every file also gets a **build provenance attestation**: a signed record, kept by GitHub, that
Actions built it from your repository at the tagged commit. It's what lets the registry trust that
a release came from your source. Anyone can check a file with
`gh attestation verify <file> --repo <you>/<repo>`.

Once that release exists, anyone can install it with **Install from GitHub…** on the Plugins page
or `trunk-pro plugin install https://github.com/you/trunk-plugin-pager`, checked against its
`SHA256SUMS` and marked **Not reviewed**. The recorder refuses a release without `SHA256SUMS` or the
manifest file, and one whose id is already a different plugin's in the registry.

The release builds cross-compile. Pure Rust dependencies make that easy; C libraries don't:

- **HTTP**: use a client with `rustls` for TLS (`ureq`, or `reqwest` with `rustls-tls`), not
  OpenSSL.
- **Other programs**: don't assume `curl`, `python` or `ffmpeg` are on the user's computer. If you
  need one, check for it in `start` and say what to install.

## Publishing to the registry

Releasing on GitHub doesn't put your plugin in front of users; the registry does. The plugin store
(the Plugins page, and `trunk-pro plugin search | install | update`) lists the plugins in
[github.com/TrunkRecorder/plugins](https://github.com/TrunkRecorder/plugins). Each plugin has an
entry, `plugins/<id>.json`, that pins one release by checksum, so what users get is what was
reviewed:

```json
{
  "id": "openmhz",
  "name": "OpenMHz",
  "description": "Uploads recorded calls to OpenMHz.",
  "repository": "https://github.com/TrunkRecorder/trunk-plugin-openmhz",
  "homepage": "https://openmhz.com",
  "license": "GPL-3.0-or-later",
  "tier": "official",
  "version": "0.1.1",
  "api": 1,
  "tag": "v0.1.1",
  "commit": "669ea2d10b193728b82276ec9ed77c81118f164f",
  "assets": {
    "x86_64-unknown-linux-gnu": {
      "url": "https://github.com/TrunkRecorder/trunk-plugin-openmhz/releases/download/v0.1.1/openmhz-0.1.1-x86_64-unknown-linux-gnu.tar.gz",
      "sha256": "74c54539…"
    },
    "aarch64-unknown-linux-gnu": { "url": "…", "sha256": "…" },
    "universal-apple-darwin": { "url": "…", "sha256": "…" },
    "x86_64-pc-windows-msvc": { "url": "…", "sha256": "…" }
  }
}
```

`tier` is `official` for plugins maintained with the recorder and `community` for everyone else's;
the store shows the difference. `commit` is the commit the tag pointed at when the entry was added:
a tag can be moved, so the commit records which source was reviewed. `index.json` gathers every
entry, and is what the recorder fetches.

To get listed (the registry's README has the details):

1. Release your plugin with the template's release workflow.
2. Fork the registry and write your entry with its scripts (they need Python 3.9 or later and the
   GitHub CLI, `gh`). You don't write it by hand:

   ```bash
   ./add-release https://github.com/you/trunk-plugin-pager v0.1.0
   ./check plugins/pager.json
   ```

   `add-release` writes `plugins/<id>.json` from the release's manifest and `SHA256SUMS`, and
   rebuilds `index.json`; `check` downloads every file and checks it.
3. Open a pull request with both files.

CI on the pull request checks the entry's fields, that every URL is a download from your
repository at the tag, that the tag still points at the commit, every file's SHA-256 and build
provenance (`gh attestation verify`), and that the Linux build's `--describe` agrees with the
entry. A maintainer then reviews the rest:

- **The source is public** and licensed for listing, and the release was built from it by GitHub
  Actions, not uploaded from someone's computer.
- **It does what it says**, and nothing else. In particular, it sends nothing anywhere the user
  didn't configure.
- **Secrets are marked** `x-secret`, and never logged.
- **It copes without M4A** if it asks for it ([Audio](#audio)).
- **It starts with its default settings**, or says in `start` what to set.
- **The id is fitting**, and not taken.

An update is the same steps with the new tag, and keeps its tier. Users see **Update to vX** on the
Plugins page, or run `trunk-pro plugin update`, when they choose; their settings carry over.

How the recorder installs a plugin: it downloads the archive for its platform from the entry's
`url`, checks its SHA-256 against the entry (and stops if they differ), unpacks it into
`plugins/<id>/` beside its config, runs `<id> --describe` and checks that the id and version match
the entry and that it supports the plugin's `api`. The user then sets it up and turns it on.

The recorder fetches the registry's list when the store first needs it, and uses that copy for 15
minutes before fetching it again (60 seconds after a fetch that failed). **Check again** on the
Plugins page fetches it at once, as does each `trunk-pro plugin search | install | update`, so a
new entry, once merged, shows in a running recorder within about 15 minutes (GitHub caches the file
for a few minutes too). When the registry can't be reached, the recorder uses the list it fetched
last, and failing that the copy built into the app at release time, so the store works offline.

A plugin doesn't have to be in the registry to run: it can be installed from its GitHub release
([Releasing](#releasing)), and **Add from a file…** runs any executable.

## Plugins in other languages

Nothing requires Rust: any executable that answers `--describe` with a manifest and speaks the JSON
lines protocol on stdin and stdout works. Both sides ignore message types and fields they don't
know. [Plugin protocol](../developer/plugins/protocol.md) has the wire format and every message,
and the SDK's `protocol` module is its reference. For the plugin store, you'd still need releases
with the same file names, `SHA256SUMS` and manifest file that the template's workflow makes.
