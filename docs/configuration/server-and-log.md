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
| `hostname` | | `""` | string | The name this computer is reached by over HTTPS, the one its certificate is for (`"radiobox.tail1234.ts.net"`). The log and the browser the app opens use it; [passkeys](#passkeys) work on it (besides `localhost`); bound to `127.0.0.1`, the recorder answers to it too (a proxy in front, `tailscale serve`, [Cloudflare Tunnel](#cloudflare-tunnel-and-access)). A port after it (`"radio.example.com:8443"`) says where the dashboard is reached when a proxy in front serves it on a port other than 443: passkeys work at that address alone |
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
whenever it restarts. A dashboard left open is closed when its session expires, and asks to sign in again.

**Signing out everywhere.** A browser that may have been left signed in somewhere (a lost phone, a
shared computer): click your name in the top bar → **Sign out everywhere**, or, as an Admin, a
user's **Sign out** in Setup → **Access** (or `trunk-pro access signout <name>`). Every browser
signed in as them until then signs in again, this one too, and their open dashboards close. It
ends sessions, not credentials: a program that sends the access token with each request
(`Authorization: Bearer`, `?token=`) keeps working, and so does anyone who has the token or a
passkey. For a token that may have been seen, make it again (remove the user and add them, or
remove the token); for a device that's gone, remove its passkey (every user's are listed in
**Access**, by name and the day each was added).

**Plain HTTP from another computer.** Setup → **Access** says **Unencrypted** when the page was
opened over plain HTTP from another computer: the token you paste and the sign-in cookie cross
the network readable, to anyone who can watch it. Over a VPN (Tailscale, WireGuard) that's fine;
otherwise turn on [HTTPS](#https).

On a recorder with no browser nearby, use the command line, then restart the recorder:

```sh
trunk-pro access add luke                    # control by default; prints the token's secret once
trunk-pro access add kiosk --access listen
trunk-pro access on
trunk-pro access list                        # on or off, anonymous, the proxy, and each user
trunk-pro access remove kiosk                # signs out whoever signed in as kiosk
trunk-pro access signout luke                # every browser signed in as luke signs in again
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
| `proxy` | off | A reverse proxy in front that signs people in: `from`, `header` (or `cloudflare`), `users`, `other`. See [Behind a proxy that signs people in](#behind-a-proxy-that-signs-people-in) |
| `users` | `{}` | By name: `access` (`"listen"` or `"control"`) and what they sign in with: `token`, `hash` (the secret's SHA-256) and `created` (Unix s); `passkeys`, each `name`, `created`, `id` (the credential's, base64url) and `key` (its public key and counter, as the recorder's WebAuthn library keeps it); `signedOut` (Unix s), when they were last [signed out everywhere](#access): browsers' sessions from then or before don't count. Made in Setup or with `trunk-pro access add`, not by hand; a passkey can be renamed or removed by hand |

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
round.

**One address: the dashboard's.** The recorder takes a passkey's answer only from the built-in
interface's own page, at an address it works out itself, never from what a request says:

| Signed in at | Passkeys work at |
|---|---|
| `localhost` | `http://localhost:<port>` (`https://` with [HTTPS](#https) on), the built-in interface's port as the recorder listens on it |
| `server.hostname` | `https://<hostname>`: the port written in `hostname` if it has one (`"radio.example.com:8443"`); else the built-in interface's port when the recorder serves HTTPS itself; else 443 (a proxy or tunnel in front serves the HTTPS) |

- **Not on an interface's port.** Interfaces are other people's pages, and a sign-in counts on every
  port of the name, so an interface's page could otherwise have your passkey sign its own server in
  with your role. On an interface's port the sign-in page offers **Passkey ↗** instead, which opens
  the dashboard's sign-in page; signed in there, the interface's port is signed in too (the cookie
  covers the name, whatever the port).
- **Reached at another port** (an SSH tunnel's `localhost:9000`, Docker publishing `8080` as
  `9080`, a proxy at a port `hostname` doesn't name): signing in with a passkey is refused with
  "Passkeys work at … alone", the address they do work at; tokens work as ever. Use that address,
  map the same port, or write the proxy's port in `hostname`.
- **`localhost` is a computer's own.** A passkey made at `http://localhost:8080` signs in whatever
  answers at `localhost:8080` on that computer: an SSH tunnel's or Docker's port mapped to that
  number is, to the browser, the same place.

**Adding one.** Signed in (with your token, the first time), open Setup → **Recording** →
**Access**: your row has **Add passkey**, with a name for it (the browser and system, unless you
change it). The browser asks to make it; it's saved at once. Each user adds their own, on their own
device: an Admin can't add one for someone else, but can remove anyone's. With a passkey, you may
remove your token (the **×** by its date), as long as you can still sign in.

**Signing in.** The sign-in page has **Passkey** above the token field; browsers that fill in
passkeys also offer them from the field itself. No name to type: the passkey says whose it is.
A sign-in waits with the browser while you answer (signed by the recorder, good for 5 minutes,
and good once), so opening the sign-in page again and again keeps nothing on the recorder and
can't push anyone else's out.

The config keeps each passkey's public key, never anything secret. A passkey kept on one device
(a security key) counts its uses, and the count is saved when it goes up (never down), so a copy
is refused; if the count can't be saved (the config can't be written), the sign-in is refused
too. Passkeys synced between devices don't count, so signing in with them writes nothing.

**Locked out?** On the recorder's computer, `trunk-pro access off`, then restart the recorder.
Open Setup, add a new Admin, and turn **Sign-in** back on. (`--config file.json` for a config
somewhere else.)

**HTTPS.** On plain HTTP the secret crosses the network in the clear, as everything else does.
That's fine on your own network, over a VPN or an SSH tunnel. Otherwise turn on [HTTPS](#https),
or put a reverse proxy that adds it in front. Across the internet, don't open the port: use a VPN
or tunnel (see [Remote access](../guides/interface.md#remote-access), and [Cloudflare
Tunnel](#cloudflare-tunnel-and-access)).

### Behind a proxy that signs people in

If a reverse proxy in front of the recorder already signs people in (Authelia, Authentik,
oauth2-proxy, `tailscale serve`, Caddy's `forward_auth`), the recorder can take its word for who
they are, so nobody signs in twice. The proxy passes the name in a header; the recorder believes
that header only from the proxy's own addresses. Cloudflare Access is different: the recorder
checks the token it signs, so it needs no addresses (see [Cloudflare Tunnel and
Access](#cloudflare-tunnel-and-access)). Off unless set; it does nothing while signing in is off.

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
| `from` | (needed) | The proxy's addresses, as the recorder sees its connections: IPs or CIDR blocks (`"127.0.0.1"`, `"10.0.0.0/8"`, `"fd7a:115c:a1e0::/48"`). From any other address the header is ignored, and the log says so once for each address (a proxy at an address not listed shows there). An entry that's neither isn't saved, nor is a block of every address (`"0.0.0.0/0"`, `"::/0"`): anyone could send the header, the same as signing in turned off. A proxy set so by hand signs nobody in (the log says why as the recorder starts). Not needed (and not used) with `cloudflare` |
| `header` | `"Remote-User"` | The header the proxy puts the name in. Not used with `cloudflare` |
| `cloudflare` | off | Cloudflare Access in front: `{ "team": "myteam", "aud": "…" }`. The name is the email in its signed token, checked; `from` and `header` aren't used. See [Cloudflare Tunnel and Access](#cloudflare-tunnel-and-access) |
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
- In the app: Setup → **Recording** → **Access** → **Proxy** sets `from`, `header` (or, for
  **Cloudflare**, **Team** and **Audience**) and **Others** (`other`); `users` is edited in the config file.

**Which header.** `Remote-User` is Authelia's. The others name it differently, so set `header` to match:

| Proxy | `header` | Notes |
|---|---|---|
| Authelia | `Remote-User` (also `Remote-Email`, `Remote-Name`, `Remote-Groups`) | Its forward-auth (nginx `auth_request`, Traefik `forwardAuth`, Caddy `forward_auth`) must pass them on, as Caddy's does with `copy_headers` |
| Authentik | `X-authentik-username` (or `X-authentik-email`) | Its proxy outpost's forward-auth. Caddy's `forward_auth` needs them in `copy_headers`, capitalised: `X-Authentik-Username` |
| oauth2-proxy | `X-Forwarded-Email` (or `X-Forwarded-User`, `X-Forwarded-Preferred-Username`) | `--pass-user-headers` (on by default) in reverse-proxy mode; with nginx `auth_request`, `--set-xauthrequest` gives `X-Auth-Request-Email` (or `-User`) and the proxy must copy it over with `proxy_set_header` |
| Cloudflare Access | none: set `cloudflare` | The recorder checks Access's signed token (`Cf-Access-Jwt-Assertion`) and ignores its plain email header. See [Cloudflare Tunnel and Access](#cloudflare-tunnel-and-access) |
| `tailscale serve` | `Tailscale-User-Login` | `luke@github` or an email. Serve strips its own headers from what clients send. Only tailnet requests carry them: none from Funnel, none from tagged devices. `from`: `127.0.0.1` and `::1` |

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

### Cloudflare Tunnel and Access

[Cloudflare Tunnel](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/)
puts the recorder at a name of yours (`https://radio.example.com`) with no port opened: `cloudflared`,
on the recorder's computer or beside it in Docker, connects out to Cloudflare, and Cloudflare
passes requests for that name back down it to `http://localhost:8080`. Cloudflare serves the
HTTPS, so the recorder needs no `server.tls`. WebSockets (the dashboard's live updates and audio)
work through it as they are.

**1. The tunnel.** Either way, the name ends up pointing at the recorder's plain-HTTP port.

- *From the dashboard* (a remotely-managed tunnel): in Zero Trust, create a tunnel, run the
  `cloudflared` command it shows with its token, then add a published application (public
  hostname): your name, service URL `http://localhost:8080`.
- *From the command line* (a locally-managed tunnel):

  ```bash
  cloudflared tunnel login
  cloudflared tunnel create radio
  cloudflared tunnel route dns radio radio.example.com
  cloudflared tunnel run radio
  ```

  with `~/.cloudflared/config.yml`:

  ```yaml
  tunnel: <the tunnel's UUID>
  credentials-file: /home/you/.cloudflared/<the tunnel's UUID>.json
  ingress:
    - hostname: radio.example.com
      service: http://localhost:8080
    - service: http_status:404      # the catch-all rule a config file must end with
  ```

  `cloudflared service install` keeps it running as a service (on Linux, with `sudo`, give it the
  config: `sudo cloudflared --config /home/you/.cloudflared/config.yml service install`).

**2. The recorder.** Set `hostname` to the tunnel's name, and keep `bind` at `127.0.0.1` so only
programs on this computer (cloudflared among them) reach the port:

```json
"server": { "bind": "127.0.0.1", "port": 8080, "hostname": "radio.example.com" }
```

- Without `hostname`, a recorder bound to localhost refuses the tunnel's requests ("this server only
  answers to localhost"): they're for `radio.example.com`, which it doesn't know as its own.
- Cloudflare tells the recorder the browser came over HTTPS (`X-Forwarded-Proto: https`), so the
  sign-in cookie is `Secure`. [Passkeys](#passkeys) work at `https://radio.example.com` (the
  recorder serves no HTTPS itself, so the proxy in front does, at 443).
- Turn [signing in](#access) on: through a tunnel, the recorder is on the internet. Tokens and
  passkeys work through the tunnel with nothing else set up.

**In Docker**, run `cloudflared` as a second service beside the recorder, and point the tunnel at
the recorder's service name instead of localhost (service URL `http://trunk-pro:8080`, the name in
[docker-compose.yml](../install/docker.md)). Drop the recorder's `ports:` then, or keep it to
`"127.0.0.1:8080:8080"`; only the tunnel needs to reach it:

```yaml
  cloudflared:
    image: cloudflare/cloudflared:latest
    restart: unless-stopped
    command: tunnel --no-autoupdate run --token ${TUNNEL_TOKEN}   # the dashboard tunnel's token, in .env
```

In the container the recorder listens on every address (`--bind 0.0.0.0`), so it answers to any
name; set `hostname` all the same, for passkeys and the URLs it logs.

**3. Access (optional): Cloudflare signs people in.** With an
[Access application](https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/self-hosted-public-app/)
in front, people sign in at Cloudflare (with the identity providers you give it) before a
request reaches the tunnel, and the recorder can take Cloudflare's word for who they are:

1. In Zero Trust → **Access controls** → **Applications**, add a **Self-hosted** application for
   `radio.example.com`, with a policy for who may in (an application lets nobody in until one does).
2. Open it again and copy its **Application Audience (AUD) Tag**.
3. Set `proxy.cloudflare` to your team name (the `myteam` of `myteam.cloudflareaccess.com`) and
   that tag, and map the emails Access signs people in with to the recorder's users:

```json
"auth": {
  "enabled": true,
  "users": { "luke": { "access": "control" }, "kiosk": { "access": "listen", "token": { "hash": "…", "created": 1763000000 } } },
  "proxy": {
    "cloudflare": { "team": "myteam", "aud": "4714c1358e65fe4b408ad6d432a5f878f08194bdb4752441fd56faefa9b2b6f2" },
    "users": { "luke@example.com": "luke" },
    "other": "listen"
  }
}
```

Or in the app: Setup → **Recording** → **Access** → **Proxy** → **Set up** → **Cloudflare**.

- **The token, not the header.** Access sends each request with the email it signed in
  (`Cf-Access-Authenticated-User-Email`) and a token it signed (`Cf-Access-Jwt-Assertion`).
  Anything that reaches the port can send the email header, cloudflared's neighbours on
  `127.0.0.1` included, so the recorder ignores it and, as
  [Cloudflare advises](https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/authorization-cookie/validating-json/),
  checks the token: signed (RS256) by one of your team's keys, issued by your team, for this
  application's Audience tag, and not expired. Then the email in it is the name, mapped through
  `users` and `other` as any proxy's is. No `from` is needed: the signature is the proof.
- **The team's keys** are fetched from `https://myteam.cloudflareaccess.com/cdn-cgi/access/certs`
  when first needed, kept, and fetched again hourly and for a token signed by a key not kept
  (Cloudflare rotates them), at most once a minute. The recorder must reach that address. If it
  can't, Access signs nobody in (the log says so once); tokens and passkeys still work.
- A token, a session or a passkey's sign-in says who first, as for any proxy; another site's page
  gets nobody. Signing out is Cloudflare's (its session lasts as long as the application says). A
  dashboard open with Access's token is closed when that token expires (a minute's leeway), and
  reconnects with the one the browser has then.
- A service token's requests carry no email, so they sign nobody in; give a program an access
  token of the recorder's instead.
- A token refused (another application's, expired, a clock far off) is logged once a reason, so an
  Audience tag pasted wrong shows in the log.

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
- **Cookies** from [signing in](#access) are `Secure` over HTTPS.
- **HSTS** (`hsts`, off by default; Interfaces → Ports → **HSTS**): browsers that have been here
  over HTTPS go straight to `https://` for a year after, even for an `http://` address typed in,
  so nothing is ever sent in the clear first. Off, a typed `http://` address sends its first
  request in the clear before the redirect. It's off by default because it binds the whole host
  name, every port: other programs on this computer serving plain HTTP at the same name (a
  router page, another web app) stop opening in those browsers, and turning the recorder's HTTPS
  off again leaves its own port unreachable to them until the year is out. Sent over the
  recorder's own HTTPS alone; behind a proxy that serves the HTTPS, set it at the proxy.
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

**No certificate at all:** behind [Cloudflare Tunnel](#cloudflare-tunnel-and-access) (or another
proxy that serves HTTPS), the recorder stays on plain HTTP and the proxy serves the certificate.

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
