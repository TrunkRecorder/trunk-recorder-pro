# Server, Log and Monitor

This page lists the keys for the web server (`server`, [signing in](#access) with
`server.auth` and [HTTPS](#https) with `server.tls`), the log (`log`) and the dashboard's internet checks (`monitor`). For remote access, security and building your own interfaces, see the
[interface guide](../guides/interface.md); for the command-line flags that override some of these
for one run, see [command-line.md](../command-line.md).

```json
"server": { "bind": "127.0.0.1", "port": 8080, "autoStart": true },
"log": { "level": "info", "file": true, "dir": "logs" },
"monitor": { "checkHosts": ["192.168.1.1:443"], "timeServer": "pool.ntp.org", "clockWarnMs": 250 }
```

## Server

The built-in interface's address and port, and the other interfaces, each on a port of its own. In
the app: **Record when the app starts** on Setup's **Recording** tab; the rest (HTTPS's files
too) on the **Interfaces** page.

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `bind` | | `"127.0.0.1"` | string | The address every interface listens on. `"127.0.0.1"`: this computer only. `"0.0.0.0"`: reachable from other machines. With [signing in](#access) off (the default), anyone who can reach the built-in interface's port can change settings and run plugins. `--bind` overrides it for one run. Config only |
| `port` | | `8080` | integer | The built-in interface's port. `--port` overrides it for one run |
| `autoStart` | | `false` | bool | Start recording when the app starts, as `--start` does. For headless machines and after a reboot |
| `allowedOrigins` | | `[]` | array of strings | Web pages on other origins allowed to use the built-in interface's API, with control (a page served by something else, such as a dev server): `"http://host:port"` as the browser sends it, `"null"` for a page opened from a file, or `"*"` for any. Each interface's own pages, and programs that aren't browsers, always may. With [signing in](#access) on, such a page must also send an access token |
| `interfaces` | | `{}` | object | The other interfaces, by id. See below |
| `auth` | | off | object | Signing in: who may, with what role. See [Access](#access) |
| `tls` | | off | object | HTTPS on every interface's port: `cert` and `key`, PEM files. See [HTTPS](#https) |
| `hostname` | | `""` | string | The name this computer is reached by over HTTPS, the one its certificate is for (`"radiobox.tail1234.ts.net"`). The log and the browser the app opens use it; [passkeys](#passkeys) work on it (besides `localhost`); bound to `127.0.0.1`, the recorder answers to it too (a proxy in front, `tailscale serve`) |
| `trpApi` | | `""` (https://trunkrecorder.pro) | string | Where the trunkrecorder.pro API is, for **Identify** and [sharing with the directory](systems.md#sharing-with-the-directory). Only for testing another copy of it (`"http://localhost:8788"`) |

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

## Access

Signing in is off unless you turn it on. Off, whoever reaches a port gets what that port gives:
control on the built-in interface's, each interface's own `access` on its. On, they also sign in as
a **user**, with that user's **access token** or one of their [passkeys](#passkeys), and get the
lower of the port's access and the user's role.

**Turning it on.** In the app: Setup → **Recording** → **Access**. Turn the **Sign-in** switch on.
With no Admin yet, it asks for one first: give it a name, **Create**, and copy the token's secret
while it's shown (once). The browser is signed in with it, and only then is signing in turned on,
so you can't lock yourself out. (With an Admin already there, it asks for their token's secret
instead, or offers **New Admin token**.) Other browsers get the sign-in page: paste the secret. One sign-in covers the
dashboard and every interface's port on that host name, for 30 days or until **Sign out**.

The browser keeps a session, not the token: who signed in, with which token (or passkey), and until when,
signed with a key the recorder keeps in `session-key` beside the config (made the first time,
readable by its owner alone, never in the config). Removing the user, making their token again
or removing the passkey ends their sessions; deleting `session-key` (then restarting the recorder) signs every browser
out. If the recorder can't keep the file it says so in the log, and browsers are signed out
whenever it restarts.

On a recorder with no browser nearby, use the command line, then restart the recorder:

```sh
trunk-pro access add luke                    # control by default; prints the token's secret once
trunk-pro access add kiosk --access listen
trunk-pro access on
trunk-pro access list                        # on or off, anonymous, the proxy, and each user
trunk-pro access remove kiosk                # signs out whoever signed in as kiosk
trunk-pro access passkey remove luke iPhone  # one of luke's passkeys, by its name
```

**Users.** Each has a name (who or what signs in) and a role. **New token** in Setup adds one, with
their token:

| Role (in Setup) | In the config | May |
|---|---|---|
| View | `listen` | See and play everything (status, calls on the air, recorded calls, live audio, the dashboard), change nothing. The config it sees has no plugin settings, key sources or users |
| Admin | `control` | Everything the built-in interface can, including adding and removing users |

The token's secret (`trp_` and 64 hex digits) is shown once, when it's made. The config keeps only
its SHA-256, so a lost secret can't be recovered: remove the user and add them again. Removing a
user signs out everyone signed in as them (from Setup at once; with `trunk-pro access remove`, at
the restart). Programs and scripts sign in with a token too; see
[Signing in](../api/README.md#signing-in) in the API docs.

**Without a token.** **None** (the default): everything but the sign-in page needs a token.
**View** (`anonymous: "listen"`): anyone who reaches the port can look and listen, and needs a
token only to change things; the dashboard's top bar then has **Sign in**, which goes to the
sign-in page (`/signin`).

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
| `enabled` | `false` | Signing in on. It needs a user with control access and a token or a passkey (or, with a [proxy](#behind-a-proxy-that-signs-people-in) set, nothing: the proxy can name them), so someone can turn it off again |
| `anonymous` | none | `"listen"`: what a request without a token may do. `"control"` is refused (it would be the same as off) |
| `proxy` | off | A reverse proxy in front that signs people in: `from`, `header`, `users`, `other`. See [Behind a proxy that signs people in](#behind-a-proxy-that-signs-people-in) |
| `users` | `{}` | By name: `access` (`"listen"` or `"control"`) and what they sign in with: `token`, `hash` (the secret's SHA-256) and `created` (Unix s); `passkeys`, each `name`, `created`, `id` (the credential's, base64url) and `key` (its public key and counter, as the recorder's WebAuthn library keeps it). Made in Setup or with `trunk-pro access add`, not by hand; a passkey can be renamed or removed by hand |

Changes in Setup apply at once: connections whose access changed are closed and reconnect with
what they have now. Changes made with `trunk-pro access` are written to the config file and take
effect when the recorder restarts.

### Passkeys

A passkey signs a browser in with the device itself (Touch ID, Face ID, Windows Hello, a phone, a
password manager, a security key): nothing to paste. It's optional; tokens keep working, for
scripts and as the way back in.

**Where they work.** Browsers allow passkeys only on `localhost`, or over [HTTPS](#https) on a
host name, never an IP address. So on the recorder's own computer at `http://localhost:8080` they
just work. From other devices, set `server.hostname` to the name the certificate is for
(`tailscale cert`'s, say) and open `https://radiobox.tail1234.ts.net:8443`. Anywhere else (an IP
address, plain HTTP on the name) the sign-in page and Setup don't offer them. A passkey belongs to
the name it was made on: one made at `localhost` isn't offered at the host name, and the other way
round; every port of that name shares it.

**Adding one.** Signed in (with your token, the first time), open Setup → **Recording** →
**Access**: your row has **Add passkey**, with a name for it (the browser and system, unless you
change it). The browser asks to make it; it's saved at once. Each user adds their own, on their own
device: an Admin can't add one for someone else, but can remove anyone's. With a passkey, you may
remove your token (the **×** by its date), as long as you can still sign in.

**Signing in.** The sign-in page has **Passkey** above the token field; browsers that fill in
passkeys also offer them from the field itself. No name to type: the passkey says whose it is.

The config keeps each passkey's public key, never anything secret. A passkey kept on one device
(a security key) counts its uses, and the count is saved when it goes up, so a copy is refused;
passkeys synced between devices don't count, so signing in with them writes nothing.

**Locked out?** On the recorder's computer, `trunk-pro access off`, then restart the recorder.
Open Setup, add a new Admin, and turn **Sign-in** back on. (`--config file.json` for a config
somewhere else.)

**HTTPS.** On plain HTTP the secret crosses the network in the clear, as everything else does.
That's fine on your own network, over a VPN or an SSH tunnel. Otherwise turn on [HTTPS](#https),
or put a reverse proxy that adds it in front. Across the internet, don't open the port: use a VPN
or tunnel (see [Remote access](../guides/interface.md#remote-access)).

### Behind a proxy that signs people in

If a reverse proxy in front of the recorder already signs people in (Authelia, Authentik,
oauth2-proxy, Cloudflare Access, `tailscale serve`, Caddy's `forward_auth`), the recorder can
take its word for who they are, so nobody signs in twice. The proxy passes the name in a header;
the recorder believes that header only from the proxy's own addresses. Off unless set; it does
nothing while signing in is off.

```json
"auth": {
  "enabled": true,
  "users": {
    "luke": { "access": "control" },
    "kiosk": { "access": "listen", "token": { "hash": "…", "created": 1763000000 } }
  },
  "proxy": {
    "from": ["127.0.0.1", "::1"],
    "header": "Remote-User",
    "users": { "luke@example.com": "luke" },
    "other": "listen"
  }
}
```

| Key | Default | Description |
|---|---|---|
| `from` | (needed) | The proxy's addresses, as the recorder sees its connections: IPs or CIDR blocks (`"127.0.0.1"`, `"10.0.0.0/8"`, `"fd7a:115c:a1e0::/48"`). From any other address the header is ignored, and the log says so once for each address (a proxy at an address not listed shows there). An entry that's neither isn't saved |
| `header` | `"Remote-User"` | The header the proxy puts the name in |
| `users` | `{}` | The proxy's names for users whose name here differs: `"luke@example.com": "luke"`. Edited in the config file, not in Setup |
| `other` | none | `"listen"`: what someone the proxy signed in may do when their name is no user here. None: they aren't signed in (the sign-in page, or `anonymous`'s access). `"control"` is refused |

- **Who they are.** The name is the user `users` maps it to, else the user of that name, with
  that user's role. They need no token or passkey. Any other name gets `other`.
- **A token or session says who first.** A request with a valid token (`Authorization: Bearer`,
  `?token=`) or a browser already signed in here (the cookie) is that user; the header counts only
  when none of those does.
- **It counts where the cookie would**: from the recorder's own pages, an address typed in or a link
  followed, and programs. Not from another site's page (even one `allowedOrigins` lets in), since
  the browser sends its sign-in at the proxy along with that page's requests too.
- **Changes apply at once.** Removing the user, changing their mapping or role, or removing the
  proxy or its address closes their connections, as for any user.
- **Signing out** is the proxy's: the dashboard shows no **Sign out** for someone the proxy signed in.
- **A way back in.** With a proxy set, an Admin with no token counts as a way in (the proxy can
  name them). If the proxy goes wrong, `trunk-pro access off` on the recorder's computer works as ever.
- In the app: Setup → **Recording** → **Access** → **Proxy** sets `from`, `header` and **Others**
  (`other`); `users` is edited in the config file.

**Which header.** `Remote-User` is Authelia's. The others name it differently, so set `header` to match:

| Proxy | `header` | Notes |
|---|---|---|
| Authelia | `Remote-User` (also `Remote-Email`, `Remote-Name`, `Remote-Groups`) | Its proxy integration must pass them on, as Caddy's `forward_auth` does with `copy_headers` |
| Authentik | `X-authentik-username` (or `X-authentik-email`) | Caddy's `forward_auth` needs them in `copy_headers`, capitalised: `X-Authentik-Username` |
| oauth2-proxy | `X-Forwarded-Email` (or `X-Forwarded-User`, `X-Forwarded-Preferred-Username`) | `--pass-user-headers` (on by default) in reverse-proxy mode; with nginx `auth_request`, `--set-xauthrequest` gives `X-Auth-Request-Email` and the proxy must copy it over |
| Cloudflare Access | `Cf-Access-Authenticated-User-Email` | Cloudflare's own advice is to verify the signed `Cf-Access-Jwt-Assertion` at the origin, which the recorder doesn't: don't trust the plain header unless nothing but Access (a Tunnel) can reach the proxy |
| `tailscale serve` | `Tailscale-User-Login` | Serve strips its own headers from what clients send. Only tailnet requests carry them: none from Funnel, none from tagged devices |

**The proxy must strip the header from what clients send.** Anything that reaches the recorder from
an address in `from` is believed, so a path through the proxy that passes a client's own
`Remote-User` along lets anyone be anyone. Authelia, Authentik and oauth2-proxy expect the proxy in
front to replace the header, and `tailscale serve` and Caddy's `forward_auth` `copy_headers` do;
on any path that doesn't (and in a plain Nginx or Caddy proxy), take it out:

```nginx
proxy_set_header Remote-User "";      # nginx: an empty value isn't sent
```

```caddy
reverse_proxy 127.0.0.1:8080 {
    header_up -Remote-User            # Caddy
}
```

With `from` set to `127.0.0.1`, every program on that computer can send the header too. With the
proxy on the same computer, keep `bind` at `127.0.0.1`, so nothing else reaches the port at all, and
set `hostname` to the name the proxy serves (bound to localhost, the recorder answers to it).

| Proxy | `header` | Notes |
|---|---|---|
| Authelia | `Remote-User` | Its forward-auth (nginx `auth_request`, Traefik `forwardAuth`, Caddy `forward_auth`) copies it from its answer |
| Authentik | `X-authentik-username` | Its proxy outpost's forward-auth; `X-authentik-email` for the email |
| oauth2-proxy | `X-Forwarded-User` | As the proxy itself, with `--pass-user-headers`. Behind nginx's `auth_request` with `--set-xauthrequest`: `X-Auth-Request-User`, copied with `proxy_set_header` |
| Cloudflare Access | `Cf-Access-Authenticated-User-Email` | An email: map it with `users`. `from` is where `cloudflared` runs; put every route to the recorder behind an Access application |
| `tailscale serve` | `Tailscale-User-Login` | `luke@github` or an email, for people on your tailnet (not Funnel's visitors). `from`: `127.0.0.1` and `::1` |

Caddy with Authelia, for example:

```caddy
radio.example.com {
    forward_auth authelia:9091 {
        uri /api/authz/forward-auth
        copy_headers Remote-User
    }
    reverse_proxy 127.0.0.1:8080
}
```

## HTTPS

Off by default: over a VPN (Tailscale, WireGuard) or on your own network, plain HTTP is fine. On,
every port the recorder serves (the built-in interface's and each interface's) speaks HTTPS with
the certificate you give it. It works with [signing in](#access) on or off.

```json
"server": { "bind": "0.0.0.0", "port": 8443, "tls": { "cert": "radiobox.crt", "key": "radiobox.key" } }
```

| Key | Default | Description |
|---|---|---|
| `cert` | | The certificate, PEM, its chain after it (Let's Encrypt's `fullchain.pem`). Absolute, `~/…`, or relative to the config file's folder |
| `key` | | Its private key, PEM (`privkey.pem`) |

- **Read when the app starts.** Turning it on or off, or pointing at other files, takes a restart.
  A pair that can't be read, isn't PEM, or whose key isn't the certificate's stops the app with
  that message (and isn't saved from the interface); it doesn't fall back to plain HTTP.
- **Renewals need no restart.** The files are looked at once a minute; a renewed pair is served
  from then on. If the new pair doesn't load, the recorder warns once and keeps serving the old one.
- **Plain HTTP to the port** is redirected to `https://` (308). `GET /api/version` still answers
  over plain HTTP, so scripts and a second launch still find the recorder.
- **Cookies** from [signing in](#access) are `Secure` over HTTPS. There's no HSTS, so you can
  turn HTTPS off again without browsers refusing the plain-HTTP port.
- The certificate must name the address you open it at (a phone at `https://radiobox.tail1234.ts.net:8443`
  needs a certificate for `radiobox.tail1234.ts.net`). Set `server.hostname` to that name, and
  the log and the browser the app opens use it; without it they use `https://localhost:…` (or the
  `bind` address), which a certificate for another name won't match.

**Getting a certificate.** The recorder doesn't make one; any of these do:

- **Tailscale**: `tailscale cert radiobox.tail1234.ts.net` writes `radiobox.tail1234.ts.net.crt`
  and `.key` (HTTPS certificates turned on in the Tailscale admin console). Run it again before
  it expires (90 days); the recorder picks up the new files.
- **Let's Encrypt**, for a name in DNS you control: [certbot](https://certbot.eff.org/) or
  [acme.sh](https://github.com/acmesh-official/acme.sh), with a DNS challenge if the recorder isn't
  reachable from the internet. Point `cert` at `fullchain.pem` and `key` at `privkey.pem`, and
  make sure the user the recorder runs as can read them.
- **mkcert**, on a home network: [mkcert](https://github.com/FiloSottile/mkcert) `radiobox.local 192.168.1.20 localhost`,
  and install its root (`mkcert -install`) on each device that opens the recorder.

A self-signed certificate works too, but every browser warns about it, and [passkeys](#passkeys)
(and other features browsers keep to secure pages) won't work with it.

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
| `checkHosts` | | `[]` | array of strings | `host:port`s the app connects to (a TCP connect, nothing sent) every `checkEveryS` to keep the network's history, with an event when one stops answering or answers again. Your router or a server on your network is a good choice. `[]` = none: the Platform page goes by the services the plugins report instead (see [What the recorder connects to](#what-the-recorder-connects-to)). **Hosts to check** on the Network card. Applies at once |
| `checkEveryS` | | `15` | number | How often each of `checkHosts` is tried, in seconds (5 at least). Applies at once |
| `networkChecks` | | `true` | boolean | `false` stops both `checkHosts` and the time server (`timeServer`) without forgetting them, for a computer whose connection something else already watches. The **Checks** switch on the Platform page's Network card. Applies at once |
| `timeServer` | | `"pool.ntp.org"` | string | The time server asked how far off the computer's clock is: one SNTP query (a 48-byte UDP packet each way, port 123 unless given as `host:port`; an IPv6 address in brackets) every 15 minutes, every 2 after it doesn't answer. It only measures; the operating system's time service keeps the clock. A server on your network (your router, a GPS clock) works too. A server that doesn't answer isn't counted as the network down. `""` = not asked. **Time server** on the Network card. Applies at once |
| `clockWarnMs` | | `250` | number | The clock off by this many milliseconds or more: the Platform light turns amber, an event says so, and calls are marked `clock_off`. Red at 5 s, or 4 times this if that's more. Raise it if your calls don't need to line up with other recorders'. Applies at once. Config only |

### What the recorder connects to

For a network administrator: everything the recorder contacts over the network, and when.

| What | Where | When |
|---|---|---|
| The time server | `timeServer` (`pool.ntp.org`), UDP 123 | Every 15 minutes. `""` or the **Checks** switch off: never |
| Hosts to check | `checkHosts`, TCP | Every `checkEveryS`. None by default |
| Plugins | The services you set them up for (Broadcastify Calls, OpenMHz, an MQTT broker…) | As they send calls. Each plugin reports how its services answer, and the Platform page's **Connections** list shows them without any traffic of its own |
| The plugin and interface lists | `raw.githubusercontent.com`, and GitHub releases to install | When you open the store or install something |
| FCC license lookups | `trunkrecorder.pro` | When you use **Identify** |
| The systems directory | `trunkrecorder.pro` | Only for a system whose **Share** switch is on |
| The capture corpus | `trunkrecorder.pro`, `iq.trunkrecorder.pro` | Only when you send a capture |

Up to 0.3.5 the recorder checked `1.1.1.1:443` and `dns.google:443` every 15 s by default. On networks that
watch for DNS over HTTPS, that looked like DoH (#148). It no longer contacts anything unless something above
asks for it.
