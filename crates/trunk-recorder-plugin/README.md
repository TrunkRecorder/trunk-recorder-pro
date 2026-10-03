# trunk-recorder-plugin

Write plugins for [Trunk Recorder Pro](https://github.com/TrunkRecorder/trunk-recorder-pro)
in Rust.

A plugin is a small program of its own. The recorder starts it, tells it what
happens (a call starts, a recorded call lands on disk, a radio registers, …),
and stops it. Plugins watch; they don't change what gets recorded. Uploaders
(OpenMHz, Broadcastify), streamers, loggers and notifiers are all plugins.

The recorder and a plugin talk in JSON lines over stdin and stdout, so a
plugin can be written in any language. This crate is the Rust side of that
protocol, plus an SDK that does the plumbing for you:

- **`Plugin` and `run`**: implement a trait, call `run` from `main`. The SDK
  reads the recorder's messages, parses your settings and answers `--describe`.
- **Settings forms**: derive `JsonSchema` on your settings struct, and the
  recorder draws a form for it on its Plugins page.
- **`CallQueue`**: an upload queue with retries and backoff, which saves the
  calls still waiting at shutdown and picks them up at the next start. It
  reports its own health to the recorder's dashboard: queue depth, upload
  time, and whether the service is up, degraded or down (name it with
  `QueueOptions::endpoint`).
- **`Host::metrics`**: report the same yourself (`Metrics`: queue, timing,
  bytes sent, the services you talk to, and any figures of your own in
  `extra`) when you don't use the queue.
- **`TalkgroupFilter`**: Trunk Recorder's talkgroup allow and deny patterns.
- **`Multipart`**: a `multipart/form-data` body for any HTTP client.
- **`testing`**: run a plugin without the recorder, and a mock web server to
  point it at.

```rust
use trunk_recorder_plugin::{topic, ConcludedCall, Host, Manifest, NoConfig, Plugin, Setup};

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
    }
}

fn main() {
    trunk_recorder_plugin::run::<Hello>();
}
```

## Getting started

Start from the [plugin template](https://github.com/TrunkRecorder/trunk-plugin-template).
It's a working plugin with tests, CI and a release workflow that builds for
every platform the recorder runs on, and its `docs/` folder is the guide to
writing plugins: the events, the settings form, uploading, testing and
releasing.

Complete plugins to learn from:
[OpenMHz](https://github.com/TrunkRecorder/trunk-plugin-openmhz),
[Broadcastify Calls](https://github.com/TrunkRecorder/trunk-plugin-broadcastify),
[Rdio Scanner](https://github.com/TrunkRecorder/trunk-plugin-rdioscanner),
[simplestream](https://github.com/TrunkRecorder/trunk-plugin-simplestream) and
[upload-script](https://github.com/TrunkRecorder/trunk-plugin-upload-script).

## Features

`sdk` (on by default) is everything for writing a plugin. The recorder itself
uses only the protocol types, with `default-features = false`.

## Versions

The crate follows semver. The protocol's `API_VERSION` is separate: it changes
only when an older recorder can't run a newer plugin.

## License

Either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option,
so your plugin can be licensed however you like. (The recorder itself is
GPL-3.0.)
