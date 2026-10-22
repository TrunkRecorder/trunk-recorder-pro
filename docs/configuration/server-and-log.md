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

The built-in interface's address and port, and the other interfaces, each on a port of its own. In
the app: **Start recording when the app starts** on Setup's **Recording** tab; the rest on the
**Interfaces** page.

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `bind` | | `"127.0.0.1"` | string | The address every interface listens on. `"127.0.0.1"`: this computer only. `"0.0.0.0"`: reachable from other machines. **There's no login**: anyone who can reach the built-in interface's port can change settings and run plugins. `--bind` overrides it for one run. Config only |
| `port` | | `8080` | integer | The built-in interface's port. `--port` overrides it for one run |
| `autoStart` | | `false` | bool | Start recording when the app starts, as `--start` does. For headless machines and after a reboot |
| `allowedOrigins` | | `[]` | array of strings | Web pages on other origins allowed to use the built-in interface's API, with control (a page served by something else, such as a dev server): `"http://host:port"` as the browser sends it, `"null"` for a page opened from a file, or `"*"` for any. Each interface's own pages, and programs that aren't browsers, always may |
| `interfaces` | | `{}` | object | The other interfaces, by id. See below |
| `trpApi` | | `""` (https://trunkrecorder.pro) | string | Where the trunkrecorder.pro API is, for the survey's **Identify surveyed sites**. Only for testing another copy of it (`"http://localhost:8788"`) |

`bind` and `port` are read when the app starts; the rest apply at once.

### `interfaces`

Interfaces besides the built-in one: web pages, installed from the interface store or folders of
your own, each served on a port of its own with the recorder's API beside it. See
[Interfaces](../interfaces/README.md).

```json
"interfaces": {
  "trunkamp": { "port": 8081, "enabled": true, "access": "listen" },
  "wall": { "port": 8082, "enabled": true, "access": "control", "path": "~/wall-display" }
}
```

The key is the interface's id: lowercase letters, digits and `-`.

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `port` | | the next free one | integer | Its port: not the built-in interface's, nor another interface's. Missing or `0`: the first free port above `server.port` is given it when the config is loaded |
| `enabled` | | `true` | bool | Served. `false`: kept, but not served |
| `access` | | `"listen"` | string | What its pages may do with the API. `"listen"`: see what the recorder does and play its calls, change nothing (the config they see has no plugin settings or key sources). `"control"`: everything the built-in interface can, including changing settings, installing plugins and quitting. Any case; anything else makes the file unreadable |
| `path` | | `""` | string | A folder of your own holding its `index.html`: absolute, `~/…`, or relative to the config file's folder. Empty: the copy installed from the store, in `interfaces/<id>/` beside the config |

Changes take effect at once: an interface is started, stopped or moved to its new port without a
restart. A config from 0.1.13 or earlier, with `interfaces` as a list of `{name, path}`, is read as this
map, each given a port; its `home` is dropped (`/` on the built-in interface's port is always the
built-in interface).

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
| `color` | | `""` | string | ANSI colour: `"console"`, `"logfile"`, `"all"` or `"none"` (any case; anything else is refused when the config is read). `""` = on the console when it's a terminal and the `NO_COLOR` environment variable isn't set |
| `frequencyFormat` | | `"mhz"` | string | How frequencies read: `"mhz"` (857.987500 MHz), `"hz"` (857987500 Hz) or `"exp"` (8.579875e+08) |
| `talkgroupDisplayFormat` | | `"id"` | string | How talkgroups read: `"id"` (3747), `"id_tag"` (3747 (DCFD Disp)) or `"tag_id"` ((DCFD Disp) 3747) |
| `statusAsString` | | `true` | bool | States as words ("Monitoring"), not numbers |
| `controlWarnRatePerS` | | `10` | number | Log an error when a control channel decodes fewer messages a second than this. `-1`: log the rate every time, as info |

The values of `level` (`"warn"` is `"warning"`), `frequencyFormat` and `talkgroupDisplayFormat` are
read in any case, and `""` is the default; anything else makes the file unreadable.

If you're coming from Trunk Recorder: the default `frequencyFormat` here is `"mhz"`, not `"exp"`.

## Monitor

What the dashboard's **Platform** page checks.

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `probeHosts` | | `["1.1.1.1:443", "dns.google:443"]` | array of strings | `host:port`s the app connects to every 15 s for the internet connection's history, with an event when the link goes down or comes back. `[]` = no checks. Read when the app starts. Config only |
