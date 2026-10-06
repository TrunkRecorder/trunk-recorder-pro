# Server, Log and Monitor

This page lists the keys for the web server (`server`), the log (`log`) and the dashboard's internet
checks (`monitor`). For remote access, security and building your own interfaces, see the
[interface guide](../guides/interface.md); for the command-line flags that override some of these
for one run, see [command-line.md](../command-line.md).

```json
"server": { "bind": "127.0.0.1", "port": 8080, "autoStart": true },
"log": { "level": "info", "file": true, "dir": "logs" },
"monitor": { "probeHosts": ["1.1.1.1:443"] }
```

## Server

The web interface, its address and port, and interfaces of your own. In Setup: **Start recording when
the app starts** on the **Recording** tab, and the **Interfaces** panel below it.

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `bind` | | `"127.0.0.1"` | string | The address the interface listens on. `"127.0.0.1"`: this computer only. `"0.0.0.0"`: reachable from other machines. **It has no login**: anyone who can reach it can change settings and run plugins. `--bind` overrides it for one run. Config only |
| `port` | | `8080` | integer | The port. `--port` overrides it for one run. Config only |
| `autoStart` | | `false` | bool | Start recording when the app starts, as `--start` does. For headless machines and after a reboot |
| `allowedOrigins` | | `[]` | array of strings | Web pages on other origins allowed to use the interface's API (an interface of your own served elsewhere): `"http://host:port"` as the browser sends it, `"null"` for a page opened from a file, or `"*"` for any. The app's own pages, and programs that aren't browsers, always may |
| `interfaces` | | `[]` | array | Interfaces of your own, each served at `/ui/<name>/`. See below |
| `home` | | `""` | string | What `/` shows: `""` = the built-in interface, or an interface's `name`. The built-in interface is always at `/builtin/`. `--ui <folder>` overrides it for one run |

`bind` and `port` are read when the app starts; the rest apply at once.

### `interfaces`

```json
"interfaces": [ { "name": "wall", "path": "~/wall-display" } ]
```

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `name` | | `""` | string | Its address, `/ui/<name>/`. Letters, digits, `-` and `_`, up to 64 characters |
| `path` | | `""` | string | The folder holding its `index.html`: absolute, `~/…`, or relative to the config file's folder |

The folder is served as it is on disk. See [docs/api](../api/README.md) for what an interface can do.

## Log

How much is logged and where to. These are Trunk Recorder's log options. In Setup: the **Log**
panel on the **Recording** tab (desktop app). Changes apply at once.

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `level` | | `"info"` | string | `"trace"`, `"debug"`, `"info"`, `"warning"`, `"error"` or `"fatal"`. Debug adds calls that weren't recorded; trace, every control message. `--log-level` overrides it for one run |
| `console` | | `true` | bool | Log to the console (stderr) |
| `file` | | `false` | bool | Also log to files in `dir`: a new one each day and at 100 MB, named as Trunk Recorder names them (`MM-DD-YYYY_HHMM_NN.log`) |
| `dir` | | `""` (= `logs`) | string | The log folder: absolute, or relative to the config file's folder |
| `syslogFriendly` | | `false` | bool | With `file`: one `trunk-pro.log`, appended to and never rotated by the app. For logrotate: SIGHUP reopens it |
| `syslog` | | `false` | bool | Also send to the system log (Linux, macOS) |
| `color` | | `""` | string | ANSI colour: `"console"`, `"logfile"`, `"all"` or `"none"`. `""` = on the console when it's a terminal and the `NO_COLOR` environment variable isn't set |
| `frequencyFormat` | | `"mhz"` | string | How frequencies read: `"mhz"` (857.987500 MHz), `"hz"` (857987500 Hz) or `"exp"` (8.579875e+08) |
| `talkgroupDisplayFormat` | | `"id"` | string | How talkgroups read: `"id"` (3747), `"id_tag"` (3747 (DCFD Disp)) or `"tag_id"` ((DCFD Disp) 3747) |
| `statusAsString` | | `true` | bool | States as words ("Monitoring"), not numbers |
| `controlWarnRatePerS` | | `10` | number | Log an error when a control channel decodes fewer messages a second than this. `-1`: log the rate every time, as info |

The values of `level`, `frequencyFormat` and `talkgroupDisplayFormat` must be spelled exactly as
listed (lower case); anything else makes the file unreadable.

If you're coming from Trunk Recorder: the default `frequencyFormat` here is `"mhz"`, not `"exp"`.

## Monitor

What the dashboard's **Platform** page checks.

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `probeHosts` | | `["1.1.1.1:443", "dns.google:443"]` | array of strings | `host:port`s the app connects to every 15 s for the internet connection's history, with an event when the link goes down or comes back. `[]` = no checks. Read when the app starts. Config only |
