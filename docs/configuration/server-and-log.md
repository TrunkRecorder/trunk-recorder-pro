# Server, log and monitor

This page lists the keys for the web server (`server`, [signing in](#signing-in) with
`server.auth` and [HTTPS](#https) with `server.tls`), the log (`log`) and the dashboard's internet checks (`monitor`). For reaching the recorder from elsewhere, signing in, proxies and certificates, see
[Remote access and signing in](../guides/remote-access.md); for building your own interfaces, the
[API docs](../api/README.md); for the command-line flags that override some of these
for one run, see [command-line.md](../command-line.md).

```json
"server": { "bind": "127.0.0.1", "port": 8080, "autoStart": true },
"log": { "level": "info", "file": true, "dir": "logs" },
"monitor": { "checkHosts": ["192.168.1.1:443"], "timeServer": "pool.ntp.org", "clockWarnMs": 250 }
```

## Server

The built-in interface's address and port, and the other interfaces, each on a port of its own. In
the app: **Record when the app starts** on Setup's **Recording** tab; who can connect directly
(`bind`) under **Direct connections** on its **Access** tab, with every way to it found listed
below; the rest (HTTPS's files too) on the
**Interfaces** page.

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `bind` | | `"127.0.0.1"` | string | The address every interface listens on. `"127.0.0.1"`: this computer only. `"0.0.0.0"`: every address, reachable from other devices. A tunnel or proxy on this computer (Tailscale Serve, Cloudflare Tunnel) reaches it either way. With [signing in](#signing-in) off (the default), anyone who can reach the built-in interface's port can change settings and run plugins. `--bind` overrides it for one run. In Setup: **Direct connections** on the **Access** tab (from the next start) |
| `port` | | `8080` | integer | The built-in interface's port. `--port` overrides it for one run |
| `autoStart` | | `false` | bool | Start recording when the app starts, as `--start` does. For headless machines and after a reboot |
| `allowedOrigins` | | `[]` | array of strings | Web pages on other origins allowed to use the built-in interface's API, with control (a page served by something else, such as a dev server): `"http://host:port"` as the browser sends it, `"null"` for a page opened from a file, or `"*"` for any. Each interface's own pages, and programs that aren't browsers, always may. With [signing in](#signing-in) on, such a page must also send an access token |
| `interfaces` | | `{}` | object | The other interfaces, by id. See below |
| `auth` | | off | object | Signing in: who may, with what role. See [Signing in](#signing-in) |
| `tls` | | off | object | HTTPS on every interface's port: `cert` and `key`, PEM files. See [HTTPS](#https) |
| `hostname` | | `""` | string | The name this computer is reached by over HTTPS, the one its certificate is for (`"radiobox.tail1234.ts.net"`). The log and the browser the app opens use it; [passkeys](../guides/remote-access.md#passkeys) work on it (besides `localhost`); bound to `127.0.0.1`, the recorder answers to it too (a proxy in front, `tailscale serve`, [Cloudflare Tunnel](../guides/remote-access.md#cloudflare-tunnel-and-access)). A port after it (`"radio.example.com:8443"`) says where the dashboard is reached when a proxy in front serves it on a port other than 443: passkeys work at that address alone |
| `trpApi` | | `""` (https://trunkrecorder.pro) | string | Where the trunkrecorder.pro API is, for **Identify** and [sharing with the directory](systems.md#sharing-with-the-directory). Only for testing another copy of it (`"http://localhost:8788"`). Config only |

`bind`, `port` and `tls` are read when the app starts; the rest apply at once.

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

The key is the interface's id: lowercase letters, digits and `-`, up to 64 characters, not starting with `-`.

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `port` | | the next free one | integer | Its port: not the built-in interface's, nor another interface's. Missing or `0`: the first free port above `server.port` is given it when the config is loaded |
| `enabled` | | `true` | bool | Served. `false`: kept, but not served |
| `access` | | `"listen"` | string | What its pages may do with the API. `"listen"`: see what the recorder does and play its calls, change nothing (the config they're sent is only what their pages show: [what View is sent](#what-view-is-sent)). `"control"`: everything the built-in interface can, including changing settings, installing plugins and quitting. Any case; anything else makes the file unreadable |
| `path` | | `""` | string | A folder of your own holding its `index.html`: absolute, `~/…`, or relative to the config file's folder. Empty: the copy installed from the store, in `interfaces/<id>/` beside the config |

Changes take effect at once: an interface is started, stopped or moved to its new port without a
restart. A config from 0.1.13 or earlier, with `interfaces` as a list of `{name, path}`, is read as this
map, each given a port; its `home` is dropped (`/` on the built-in interface's port is always the
built-in interface).

## Signing in

`server.auth`: who may sign in, with what role. Off unless turned on. How to turn it on, users and
their roles (View is `listen`, Admin is `control`), passkeys, sessions and signing out everywhere
are in [Remote access and signing in](../guides/remote-access.md#signing-in). In the app: Setup →
**Access**.

```json
"server": {
  "auth": {
    "enabled": true,
    "anonymous": "listen",
    "users": {
      "luke": { "access": "control", "token": { "hash": "…", "created": 1763000000 },
                "passkeys": [{ "name": "iPhone", "created": 1763100000, "id": "…", "key": { "cred": { … } } }] },
      "kiosk": { "access": "listen", "token": { "hash": "…", "created": 1763000000 } }
    }
  }
}
```

| Key | Default | Description |
|---|---|---|
| `enabled` | `false` | Signing in on. It needs a user with control access and a token or a passkey (or, with a [proxy](#proxy) set, nothing: the proxy can name them), so someone can turn it off again |
| `anonymous` | none | `"listen"`: what a request without a token may do. `"control"` is refused (it would be the same as off) |
| `proxy` | off | A reverse proxy in front that signs people in: `from`, `header` (or `cloudflare`), `users`, `other`. See [Proxy](#proxy) |
| `users` | `{}` | By name: `access` (`"listen"` or `"control"`) and what they sign in with: `token`, `hash` (the secret's SHA-256) and `created` (Unix s); `passkeys`, each `name`, `created`, `id` (the credential's, base64url) and `key` (its public key, as the recorder's WebAuthn library keeps it). Made in Setup or with `trunk-pro access add`, not by hand; a passkey can be renamed or removed by hand |

Changes in Setup apply at once: connections whose access changed are closed and reconnect with
what they have now. Changes made with `trunk-pro access` are written to the config file and take
effect when the recorder restarts.

Two files beside the config hold what changes as people sign in and out, never in the config:
`session-key` (what browsers' sessions are signed with) and `access-state.json` (see
[Files kept beside the config](README.md#files-kept-beside-the-config)).

### What View is sent

A View user's browser (and a `listen` interface's pages) gets the config with only what its pages
show. Everything else comes as its default or is left out:

| Sent | Withheld |
|---|---|
| Radios (`sources`) | Plugin settings and `pluginDelays`, the recorder's and each system's |
| Systems, sites, channels, signals, names | Key sources (`keys`), and which a system uses |
| Recording rules, vocoder, file name format | Users, proxy and Cloudflare settings (`server.auth`) |
| `server.port`, `hostname`, `trpApi`, `autoStart` | `server.bind`, `allowedOrigins`, `interfaces`, `tls` |
| [`storage`](storage.md) but its `url` | `storage.url` (it can hold a password) |
| `log` but its `dir`, `monitor`, capture rules | Folders and files: `recording.captureDir`, `ramSpool.dir`, `log.dir`, `diagnostics.captureDir`, a conventional system's `channelFile` |
| | Keys the recorder doesn't know (a typo, another program's) |

A setting added to the recorder later is withheld until it's decided otherwise.

### Proxy

`server.auth.proxy`: a reverse proxy in front of the recorder that already signs people in
(Authelia, Authentik, oauth2-proxy, `tailscale serve`, Caddy's `forward_auth`, Cloudflare Access).
Off unless set; it does nothing while signing in is off. How it's believed, which header each
proxy uses, and setting up Cloudflare are in
[Behind a proxy that signs people in](../guides/remote-access.md#behind-a-proxy-that-signs-people-in)
and [Cloudflare Tunnel and Access](../guides/remote-access.md#cloudflare-tunnel-and-access).

| Key | Default | Description |
|---|---|---|
| `from` | (needed) | The proxy's addresses, as the recorder sees its connections: IPs or CIDR blocks (`"127.0.0.1"`, `"10.0.0.0/8"`, `"fd7a:115c:a1e0::/48"`). From any other address the header is ignored, and the log says so once for each address (a proxy at an address not listed shows there). An entry that's neither isn't saved, nor is a block of every address (`"0.0.0.0/0"`, `"::/0"`): anyone could send the header, the same as signing in turned off. A proxy set so by hand signs nobody in (the log says why as the recorder starts). Not needed (and not used) with `cloudflare` |
| `header` | `"Remote-User"` | The header the proxy puts the name in. Not used with `cloudflare` |
| `cloudflare` | off | Cloudflare Access in front: `{ "team": "myteam", "aud": "…" }`. The name is the email in its signed token, checked; `from` and `header` aren't used |
| `users` | `{}` | The proxy's names for users whose name here differs: `"luke@example.com": "luke"`. Edited in the config file, not in Setup |
| `other` | none | `"listen"`: what someone the proxy signed in may do when their name is no user here. None: they aren't signed in (the sign-in page, or `anonymous`'s access). `"control"` is refused |

In the app: Setup → **Access** → **Proxy** sets `from`, `header` (or, for
**Cloudflare**, **Team** and **Audience**) and **Others** (`other`).

## HTTPS

`server.tls`: off by default. On, every port the recorder serves (the built-in interface's and
each interface's) speaks HTTPS with the certificate you give it. It works with
[signing in](#signing-in) on or off. Getting a certificate, and when you don't need one, are in
[HTTPS](../guides/remote-access.md#https) in the remote access guide.

```json
"server": { "bind": "0.0.0.0", "port": 8443, "tls": { "cert": "radiobox.crt", "key": "radiobox.key" } }
```

| Key | Default | Description |
|---|---|---|
| `cert` | | The certificate, PEM, its chain after it (Let's Encrypt's `fullchain.pem`). Absolute, `~/…`, or relative to the config file's folder |
| `key` | | Its private key, PEM (`privkey.pem`) |
| `hsts` | `false` | Send `Strict-Transport-Security` (a year) over HTTPS. See below |

- **Read when the app starts.** Turning it on or off, or pointing at other files, takes a restart.
  A pair that can't be read, isn't PEM, or whose key isn't the certificate's stops the app with
  that message (and isn't saved from the interface); it doesn't fall back to plain HTTP.
- **Renewals need no restart.** The files are looked at once a minute; a renewed pair is served
  from then on. If the new pair doesn't load, the recorder warns once and keeps serving the old one.
- **Plain HTTP to the port** is redirected to `https://` (308). `GET /api/version` still answers
  over plain HTTP, so scripts and a second launch still find the recorder. A plain-HTTP request
  that carried a credential (an `Authorization` header, `?token=`, the sign-in cookie, or signing
  in itself) isn't redirected: it's refused (400), since the credential already crossed the
  network readable, and the log says so once for each kind. Use `https://`, and make again an
  access token that was sent that way.
- **Cookies** from [signing in](#signing-in) are `Secure` over HTTPS.
- **HSTS** (`hsts`, off by default; Interfaces → Ports → **HSTS**): browsers that have been here
  over HTTPS go straight to `https://` for a year after, even for an `http://` address typed in,
  so nothing is ever sent in the clear first. Off, a typed `http://` address sends its first
  request in the clear before the redirect. It's off by default because it binds the whole host
  name, every port: other programs on this computer serving plain HTTP at the same name (a
  router page, another web app) stop opening in those browsers, and turning the recorder's HTTPS
  off again leaves its own port unreachable to them until the year is out. Sent over the
  recorder's own HTTPS alone; behind a proxy that serves the HTTPS, set it at the proxy.
- The certificate must name the address you open it at; set `server.hostname` to that name.

## Log

How much is logged and where to. These are Trunk Recorder's log options. In Setup: **Recording** → **Advanced** → **Log**
(desktop app; the folder and logrotate switch show once **To files** is on). Changes apply at once.

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
| `checkHosts` | | `[]` | array of strings | `host:port`s the app connects to (a TCP connect, nothing sent) every `checkEveryS` to keep the network's history, with an event when one stops answering or answers again. Your router or a server on your network is a good choice. `[]` = none: the Platform page goes by the services the plugins report instead (see [What the recorder connects to](#what-the-recorder-connects-to)). **Hosts to check** on the Network card. Applies at once |
| `checkEveryS` | | `15` | number | How often each of `checkHosts` is tried, in seconds (5 at least). Applies at once |
| `networkChecks` | | `true` | boolean | `false` stops both `checkHosts` and the time server (`timeServer`) without forgetting them, for a computer whose connection something else already watches. The **Checks** switch on the Platform page's Network card. Applies at once |
| `timeServer` | | `"pool.ntp.org"` | string | The time server asked how far off the computer's clock is: one SNTP query (a 48-byte UDP packet each way, port 123 unless given as `host:port`; an IPv6 address in brackets) every 15 minutes, every 2 after it doesn't answer. It only measures; the operating system's time service keeps the clock. A server on your network (your router, a GPS clock) works too. A server that doesn't answer isn't counted as the network down. `""` = not asked. **Time server** on the Network card. Applies at once |
| `clockWarnMs` | | `250` | number | The clock off by this many milliseconds or more: the Platform light turns amber, an event says so, and calls are marked `clock_off`. Red at 5 s, or 4 times this if that's more. Raise it if your calls don't need to line up with other recorders'. Applies at once. Config only |
| `checkUpdates` | | `true` | boolean | Ask GitHub (`api.github.com`) once a day, and at start, for the app's latest release: when it's newer than the one running, the sidebar shows **v… available** beside the version, linking to it. Nothing is downloaded or installed. `false` for a closed network or a metered link. Not paused by `networkChecks`, which is about watching the connection. **Check for updates** on the Network card. Applies at once |

### What the recorder connects to

For a network administrator: everything the recorder contacts over the network, and when.

| What | Where | When |
|---|---|---|
| The time server | `timeServer` (`pool.ntp.org`), UDP 123 | Every 15 minutes. `""` or the **Checks** switch off: never |
| The app's latest release | `api.github.com`, HTTPS | At start and once a day. `checkUpdates` off: never |
| Hosts to check | `checkHosts`, TCP | Every `checkEveryS`. None by default |
| Plugins | The services you set them up for (Broadcastify Calls, OpenMHz, an MQTT broker…) | As they send calls. Each plugin reports how its services answer, and the Platform page's **Connections** list shows them without any traffic of its own |
| The plugin and interface lists | `raw.githubusercontent.com`, and GitHub releases to install | When you open the store or install something |
| FCC license lookups | `trunkrecorder.pro` | When you use **Identify** |
| The systems directory | `trunkrecorder.pro` | Only for a system whose **Share** switch is on |
| The capture corpus | `trunkrecorder.pro`, `iq.trunkrecorder.pro` | Only when you send a capture |

By default the recorder contacts only the time server and GitHub for the latest release (each can be turned
off), the rest only once you set it up.
