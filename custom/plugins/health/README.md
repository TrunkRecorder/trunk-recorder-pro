# Health check — a Trunk Recorder Pro plugin

Checks the recorder every few minutes and says, on its card and its own **Health** page, what to
look at and what to do about it. With an [ntfy](https://ntfy.sh) topic set, it also sends phone
alerts when the box's state changes, and when the recorder goes away unexpectedly.

## What it checks

| Part | Looks at | Says, for example |
|---|---|---|
| Radios | dropped samples, clipping, peaks, samples delivered | "losing samples: move it off an add-on USB card"; "brief bursts reach full scale" |
| Control channels | share of messages decoded, lock, in the last 10 minutes and over the window | "decoding poorly"; "recovered" |
| Calls, by site | typical signal, noise, SNR and clean voice of each site's calls | "weak signal: its radio already reaches full scale, so more gain would clip" |
| Overload | clipping, failing control channels, raised noise, on each radio together | "overloaded by strong signals: turn its RF gain down a step" |
| Neighbours | a channel's raised noise next to a control channel | "50 kHz from Chestnut Ridge's control channel" (not overload) |
| Uploads | the other plugins' failures, dropped calls, restarts, queues | "Plugin rdioscanner: 12 failed (20%)" |
| Computer | disk space, CPU, memory, temperature, the decoder's load | "only 6% free: calls stop saving when it fills" |
| Clock | synchronized, measured offset | "The clock is off" |

The rules lean towards saying less: a WARN or BAD needs clear evidence, and anything that may be
something else is an INFO note that says "likely". A site is judged by all its calls, so one weak
channel is a note, not a problem.

## Where it shows

- **Its card** (Setup → Plugins, and the Plugins page): a light and one line, like
  *1 to look at: Aircraft: weak signal. 3/3 radios, 6/6 control channels, 6/7 sites' calls OK*.
- **Its Health page** (in the navigation): its settings, the **Send a test alert** and
  **Check now** buttons, and its log: each finding when it appears or changes, with the evidence
  and what to do, and "Cleared" when it goes.
- **The dashboard's Health tab**: its figures (overall, how many to look at, calls checked, disk).

## Alerts

1. Install the **ntfy** app on your phone, tap **+**, and subscribe to a topic name nobody would
   guess (`trp-` and some random letters).
2. Put the same name in **ntfy topic for alerts**, and press **Send a test alert**.

It tells you when the box turns WARN or BAD (or BAD only, as you choose), once the change has held
for two checks in a row, and again when it's back to OK. If the recorder crashes or is killed
(rather than stopped), it says so at once, and again when recording starts again.

## Settings

| Setting | Default | |
|---|---|---|
| ntfy topic for alerts | (none) | A topic name (ntfy.sh) or a full URL (your own ntfy server) |
| ntfy access token | (none) | Only for a server that needs one |
| Alert on | WARN and BAD | Or BAD only, or never |
| Checks in a row before alerting | 2 | |
| Check every | 5 minutes | |
| Look back | 30 minutes | The span each check judges |
| The recorder's web port | 0 (found) | |
| **Each system:** Don't check this system | off | Leaves its calls and control channels out (its radio is still checked) |

## How it reads the recorder

The plugin protocol gives it each site's decode rate and the recorded calls (asked of the call
store). The rest — each radio's drops, clipping and peaks, the computer, the clock, the other
plugins — it reads from the recorder's own web API on this computer, read-only, as the dashboard
does. It finds the port in the recorder's config file.

## Building

```bash
cargo build --release      # target/release/health
cargo test
```

Then, on the Plugins page, **Add from a file…** and give the path of the build. Tagging a version
(`git tag v0.1.0 && git push --tags`) runs the Release workflow, which makes the files
**Install from GitHub…** needs.

`sdk/trunk-recorder-plugin` is the plugin SDK from Trunk Recorder Pro 0.3.0 (MIT or Apache-2.0),
kept here so the plugin builds without fetching it.

## License

MIT or Apache-2.0, at your choice.
