# Testing the security layer (PR #174, branch `auth`)

What signing in, roles, passkeys, HTTPS, the proxy / Cloudflare sign-ins and
the cross-origin rules claim; how each claim is tested; what was missing and
is now covered; and what only a person with real devices can check.

Written 2026-11-14 against `7026e9c` plus the uncommitted test work.

## How to run it

| What | Command | Time | In CI |
| --- | --- | --- | --- |
| Unit tests (access, passkey, cloudflare, tls, server, protocol, config) | `cargo test -p trunk-pro` / `cargo test -p trunk-app` | ~10 s | yes (`test` job) |
| End to end, the real binary over real sockets | `cargo test -p trunk-pro --test auth_e2e` | ~5 s | yes: it's an integration test, so `cargo test --release --workspace` runs it |
| Passkeys in headless Chrome with a virtual authenticator | `cd web && npm run build && cargo build -p trunk-pro && npm run test:browser` | ~6 s | no (see below) |
| Dashboard rendering per role (mocked server) | `cd web && npm test` (`access.test.tsx`, `signin.test.ts`) | part of `npm test` | yes (`web` job) |

`npm run test:browser` (`web/scripts/auth-browser.mjs`) drives Chrome over
the DevTools Protocol with Node's own WebSocket (no new dependency). It
finds Chrome or Chromium (`CHROME=` to name one) and skips, saying so, when
there is none; `TRUNK_PRO=` names the binary (default `target/debug/trunk-pro`,
which must be built after `npm run build`: debug builds read `web/dist` from
disk, release builds embed it). `KEEP=1` keeps its temporary folder.

It isn't in CI: the `web` job builds the dashboard but not the binary, and the
`test` job builds the binary but not the dashboard (the wasm toolchain lives
in the `web` job). The cheapest place would be the `build` job's
linux-x86_64 leg after "Smoke test" (it has both, and ubuntu runners ship
Chrome): `actions/setup-node` (22, for its WebSocket) then
`TRUNK_PRO=$BIN node web/scripts/auth-browser.mjs`. Not added: it couldn't be
tried on a runner from here.

## The matrix

Each claim, the tests that prove it, and how. **B** = behaviour (a request
made, an answer checked); **S** = source text (a regex over a file, or a
list read from the source); **M** = rendered against a mocked server.
"Added" marks tests written in this pass.

### Who a request is

| # | Claim (and what must be refused) | Tests | Kind |
| --- | --- | --- | --- |
| 1 | An access token's secret signs in as its user from `Authorization: Bearer` (any case, any spaces) or `?token=`; only its SHA-256 is kept | `access::a_request_is_who_its_secret_says`, `server::signing_in_guards_every_route`, e2e `signing_in_from_the_command_line_to_the_socket` | B |
| 2 | What isn't a credential signs nobody in: another scheme, a scheme with no secret, a header that isn't UTF-8, a 64 KiB secret, `xtoken=`/`token_=`, **the config's hash sent as the secret** (a control user sees the hashes) | added `access::what_isnt_a_credential_signs_nobody_in`; e2e (hash as Bearer → 401) | B |
| 3 | Of several, the first valid secret counts (two `Authorization` headers, two `token=`); a token beats a cookie beats the proxy | added `what_isnt_a_credential…`; `a_proxy_says_who…`, `cloudflare_says_who…` | B |
| 4 | A session cookie is who it says only as this recorder's key signed it, before it expires, while the user still holds the credential it names (token remade, passkey removed, user removed: nobody) | `access::a_session_is_signed_and_ends` (8 named tamperings), `passkey::a_passkey_session_ends_with_the_passkey`, `server::signing_in_guards_every_route` | B |
| 5 | **No change at all** to a cookie or to a sealed passkey sign-in is accepted | added `access::no_change_to_a_session_or_a_sealed_sign_in_is_taken`: every one-character replacement, deletion, insertion and every prefix (~2,700 variants), deterministic | B (property) |
| 6 | Cookies keep to their recorder: the cookie's name comes from the key (two recorders on one host name) | `a_session_is_signed_and_ends`, `signing_in_guards_every_route` (logout clears only its own) | B |
| 7 | Cookies (and the proxy's header) count only from this origin's pages, typed URLs/bookmarks, programs (no `Sec-Fetch-*`) and links followed here; never same-site (another port), cross-site fetches, frames, scripts, images | `signing_in_guards_every_route` (same-site, cross-site navigation); added `server::whose_cookies_count` (table incl. iframe/frame/no-cors/websocket/unknown values) | B |
| 8 | What `allowedOrigins` (including `"*"`) lets in signs in with a token alone: its cookie and the proxy's header are ignored, for HTTP and WebSocket; an origin not allowed is refused even with a token | `signing_in_guards_every_route` (one origin, cookie); added `server::what_cors_lets_in_signs_in_with_a_token_alone` | B |
| 9 | CORS preflight answered without credentials, no `Allow-Credentials` | `signing_in_guards_every_route` | B |
| 10 | The proxy's header is believed from `proxy.from` alone, one header, not empty, mapped or `other`, re-checked against the config as it is now | `access::a_proxy_says_who_from_its_addresses_alone`, `server::a_proxy_in_front_signs_people_in`, `over_https_signing_in_is_secure`, e2e (set from the page, allowed and disallowed peer) | B |
| 11 | **A proxy set as it can't be** (`from: 0.0.0.0/0`, `other: control`, a bad block), written by hand, signs nobody in | added `access::a_proxy_set_as_it_cant_be_signs_nobody_in`, e2e (hand-edited config, restart, header ignored, startup log says why). **Was a bug**: see Findings | B |
| 12 | Cloudflare Access: RS256 only, the team's key by `kid`, issuer, audience, expiry/nbf with 60 s leeway, an email (service tokens refused); keys fetched once, refetched for an unknown `kid` at most once a minute and hourly; no keys: nobody | `cloudflare::a_token_is_checked` (`none`, HS256, no kid, forged claims, wrong key), `keys_are_fetched_kept_and_refetched_for_a_new_one`, `access::cloudflare_says_who_by_its_token`, `server::behind_cloudflare_tunnel_and_access` (offline keys) | B |
| 13 | **No change** to a token is accepted; RS384/RS512/PS256/ES256/`rs256`/no `alg` refused; `email` that's empty, blank, a list or null refused | added `cloudflare::no_change_to_a_token_is_taken` (~4,000 variants) | B (property) |
| 14 | A key Cloudflare drops stops signing in at the next fetch; a failed fetch brings nothing back, keeps the keys had | added `cloudflare::a_key_cloudflare_dropped_stops_signing_in` | B |
| 15 | Sign out everywhere ends every session signed in **at or before** that second, not one after; access tokens still work | `server::sessions_end_when_they_expire_or_are_signed_out_everywhere`; added `access::signed_out_everywhere_ends_sessions_signed_in_until_then` (the boundary); e2e (from the page; from the CLI across a restart); browser (from the top bar) | B |

### What a request may do

| # | Claim | Tests | Kind |
| --- | --- | --- | --- |
| 16 | **Every HTTP route** on both kinds of port (built-in; a listen interface's) for each caller (auth off; on, anonymous; anonymous View; View; Admin), including the fallbacks (dashboard / interface folder, and a browser's sign-in page in place of a page) | before: 7 guarded paths for anonymous, a few for View (`signing_in_guards_every_route`), api_v1's door tests. Added `server::every_route_is_decided_for_every_caller_on_both_ports`: 24 rows × 5 callers on the built-in port, 22 of them on the interface's too (230 real requests). The list of routes is read from `server.rs`/`api_v1.rs` and must equal the table's, so **a new route fails the test until it has a row** | B (completeness: S) |
| 17 | **Every WebSocket command** has a decided role (listen: subscribe, statsQuery, radioQuery, callsQuery, listen; everything else control) | before: ~10 commands spot-checked. Added `server::every_command_has_a_decided_role`: one row per command, checked against `Command::TYPES`, so **a new command fails until decided** (and `needs()` defaults to control meanwhile) | B |
| 18 | Over a real socket, every control command is refused to every kind of listen caller (View user; anonymous View; Admin through a listen port; anyone on a listen port with auth off), with the right reason, and nothing refused happens; looking is answered | before: `start` as View (`a_socket_has_its_users_access_until_it_changes`). Added `server::a_listen_socket_is_refused_every_command_but_looking` (43 commands × 4 callers); e2e (createToken/start/listDir/quit refused, callsQuery answered, no user made) | B |
| 19 | **Every message type** has a decided audience (control-only: plugins, pluginRuntime, interfaces, store, installProgress, keys) | before: `keys` only. Added `protocol::every_message_has_a_decided_audience` (exhaustive vs `ToClient::TYPES`; the control six constructed and checked) | B / registry |
| 20 | Access is the lower of the port's and the user's | `access::access_is_the_lower_of_the_port_and_the_user`; the route matrix; e2e (Admin on the wall port: listen, export 403) | B |
| 21 | A socket is closed when its access changes (user removed, role changed, passkey removed, signed out everywhere) or what it signed in with expires (cookie, Cloudflare token) | `a_socket_has_its_users_access_until_it_changes`, `a_proxy_in_front_signs_people_in`, `sessions_end_…`, `behind_cloudflare_…`; browser (passkey removed → dashboard back to the sign-in page) | B |
| 22 | Passkey routes answer on the built-in port alone, from its own pages (not even an allowed origin's), only on localhost or `server.hostname`; the one origin taken is the recorder's (port, scheme), never the request's | `passkey::passkeys_are_for_localhost_or_the_host_name…`, `on_the_host_name_the_origin_s_port_is_the_dashboard_s`, `server::passkeys_add_and_sign_in`; route matrix (403 on the wall port for every caller); browser (a real answer made on the interface's page, relayed: 401 "work at … alone") | B |
| 23 | Passkey ceremonies: an add taken once within 5 min, 2 per user; a sign-in sealed, once, within 5 min, for its RP; counter only raised, and an unsaved count refuses the sign-in | `passkey::*` (8 tests, software ES256 key), `server::a_passkey_s_count_unsaved_signs_nobody_in`; browser (counter in the config equals the authenticator's after each sign-in) | B |
| 24 | Only control users sign other users out everywhere; a proxy's user can't (signing out is the proxy's); only on the built-in port | `sessions_end_…`; added check in `what_cors_lets_in_…` (proxy user → 403) | B |

### Secrets

| # | Claim | Tests | Kind |
| --- | --- | --- | --- |
| 25 | A View user is sent no secret: plugin settings (recorder's, a system's, a conventional channel's), key sources and passwords' files, the database URL, users (token hashes, passkey records and names), proxy and Cloudflare settings; not in the greeting, a later config, `hello` (no `configPath`), `/api/session`, `/api/v1/storage`, `/api/v1/calls` | before: `access::listen_sees_no_secrets_and_changes_nothing` (`redact_config` on a hand-made JSON). Added `server::a_view_user_is_sent_no_secret` (13 marked secrets through the real greeting and the wire; the Admin's copy is checked to contain them, so the check isn't vacuous); e2e (View user's config has no `auth`, no hash) | B |
| 26 | A token's secret is shown once (`tokenCreated`, CLI stdout), never in `Debug`, never saved | `protocol::a_secret_is_sent_but_never_shown`, `server::a_user_is_made_…`; e2e (config has hashes only; `access list` shows no secret) | B |
| 27 | No secret in the log; who changed what is logged by kind | e2e: four runs' stdout+stderr searched for both tokens, the session cookies and the session key; `User luke: setConfig` present | B |
| 28 | The session key is kept beside the config, mode 0600, never in the config, survives a restart | `a_session_is_signed_and_ends`; e2e (mode, a cookie still good after a restart) | B |

### Transport and headers

| # | Claim | Tests | Kind |
| --- | --- | --- | --- |
| 29 | A key that isn't the certificate's (missing, not PEM) is refused, at start (no plain HTTP instead) and from setConfig | `tls::a_pair_that_isnt_one_is_refused_and_said_why`, `server::over_https_signing_in_is_secure`; e2e (the binary exits with the reason; Linux only, see Findings) | B |
| 30 | A renewed pair is served without a restart; a broken renewal keeps the old one | `tls::a_renewed_certificate_is_served_without_a_restart` | B |
| 31 | Plain HTTP on the HTTPS port: `/api/version` answered, a credential (Authorization, `?token=`, `trp_` cookie, login/passkey POST) refused with 400, anything else 308 | `tls::https_and_plain_http_on_one_port`; e2e on the real binary | B |
| 32 | A connection that never finishes its handshake (silent, half a request, half a ClientHello) is closed when its time is up; a plain request longer than 8 KiB is closed unanswered; the port serves on | added `tls::a_connection_that_never_finishes_is_let_go` (with a 300 ms limit through a test-only constructor; the 10 s value is a constant) ; e2e (oversized, a silent connection holds up nobody) | B |
| 33 | Over HTTPS: `Secure` cookie, every port HTTPS, HSTS only when `hsts` is set and never over plain HTTP; the socket over `wss://` | `over_https_signing_in_is_secure` (added the `wss://` part); e2e | B |
| 34 | `Referrer-Policy: no-referrer`, `nosniff` on every answer (interface ports too); `X-Frame-Options: DENY` and `no-store` on the sign-in page and the dashboard's HTML | `signing_in_guards_every_route`, `a_socket_has_its_users_access…`; e2e | B |
| 35 | DNS rebinding: bound to localhost, only localhost names (or `hostname`, or an allowed origin's host) | `server::its_own_pages…`, `other_sites_are_refused`, `allowed_origins_get_cors`, `behind_cloudflare_…` | B |

### Config validity, lockout and recovery

| # | Claim | Tests | Kind |
| --- | --- | --- | --- |
| 36 | `Auth::problem`: on needs a control user who can sign in (token, passkey, or a proxy that can name them); anonymous control refused; a bad proxy refused on or off | `config::signing_in_needs_a_way_back_out`, `a_proxy_names_users_from_its_addresses`, `cloudflare_is_a_team_and_an_audience`; setConfig refusals in `server::*` | B |
| 37 | The CLI: `access on` refused with nobody to turn it off; `add` (once per name, roles), `list` (no secrets), `remove` (not the last Admin while on), `signout`, `passkey remove` (by name; says which exist), **`access off` works whatever else is wrong** | added e2e `signing_in_from_the_command_line_to_the_socket` (every subcommand, success and refusal). **`off` was a bug**: see Findings | B |

### The pages

| # | Claim | Tests | Kind |
| --- | --- | --- | --- |
| 38 | View mode: no Setup/Plugins/Interfaces, no Start/Stop/Setup guide, no editors | `access.test.tsx` (every route rendered); browser (the real dashboard as `kiosk`) | M; B |
| 39 | The sign-in page: Passkey only at the recorder's one origin; elsewhere a link to it (only if it answers); none on an IP address; base64url | `signin.test.ts` (**regex over `signin.html`**, plus its `passkeysAt`/base64url lines extracted and run); browser (button on localhost:port; link on the interface's port; neither on 127.0.0.1) | S; B |
| 40 | Passkeys from the top bar: add (name prefilled, listed), sign in by conditional UI (autofill) and by the button, sign out everywhere | `access.test.tsx` (mocked); browser (virtual CTAP2 authenticator, resident key, UV: the real `navigator.credentials` calls in `controller.ts` and `signin.html`) | M; B |

## Findings

Fixed (small, clear):

1. **A proxy written into the config by hand as it can't be was believed.**
   `AuthProxy::problem` refuses `from: ["0.0.0.0/0"]` (and `::/0`, `other:
   control`, a bad block) when saving from the page, but a config written by
   hand (or by an earlier build of this branch, which allowed `/0`) was
   served as it was: the startup log warned, and then anyone who sent
   `Remote-User: luke` was luke. Now `access::proxied` and `Who::grants`
   ignore a proxy whose `problem()` is set (`AuthProxy::problem` made `pub`).
   Proven by `a_proxy_set_as_it_cant_be_signs_nobody_in` and the e2e test
   (both fail without the fix). The docs' `from` row says so.
2. **`trunk-pro access off` refused to turn signing in off when the proxy was
   set as it can't be** (`Auth::problem` reports the proxy's problem on or
   off), though the docs name it as the way back in "if the proxy goes
   wrong". Now `off` always works (`access/cli.rs`); `on` still checks.
3. **A session cookie's MAC could be written two ways.** `toolkit::unhex`
   used `u8::from_str_radix`, which takes `"+f"` for `0x0f`, so a cookie (or
   a sealed sign-in) with `+` in place of a `0` still verified. Harmless (the
   same MAC bytes, no new session), but it made "no change is accepted"
   false; found by the new property test. `unhex` now takes hex digits alone.
   (Upper- and lowercase hex remain the same MAC; that's documented.)

Not fixed, for a decision:

4. **Warnings in the log go to every connection, View users included**
   (`logging::to_browsers` publishes level ≥ warning as `Out::Text`). A
   plugin that logs an error with its API key in a URL, or a key file's path
   in an error, shows it to View users. Not this branch's code, but it's the
   one path by which a secret could reach a View user that the redaction
   doesn't cover. Options: plugin lines to control only, or no log for
   listen access.
5. **On macOS, a startup failure opens a modal alert** (`fatal()` runs
   `osascript display alert` when stderr isn't a terminal) that waits for
   someone to close it; a test (or a LaunchAgent) that starts the binary with
   a bad config hangs until then. The e2e test therefore checks "a bad pair
   stops it starting" on Linux only. (One such alert was raised on this Mac
   while writing the test, before it was skipped there.)
6. Known and accepted (from the review reply): sessions can't be revoked one
   by one; `X-Forwarded-Proto`/`Origin` decide the cookie's `Secure` from
   anyone; a raised counter stays in memory when its save fails; a passkey
   made on `localhost` belongs to whatever answers on that port. Paths that
   aren't credentials (TLS files, capture folder, interface folders) are in a
   View user's config by design (`hello.configPath` isn't).

Remaining automation gaps, by risk:

- (medium) The browser test isn't in CI (above); a regression in the
  dashboard's or sign-in page's WebAuthn code would be caught only locally.
- (medium) Real authenticators, sync providers, real certificates, real
  Cloudflare and proxies: the checklist below.
- (low) Windows: CI runs no tests there (the session key's file mode is
  unix-only by design).
- (low) The handshake limit's 10 s value is only read, not timed (the test
  runs the same path at 300 ms).

## Manual checklist

For what needs a person, a real browser or device, or a service. Set up
once: a recorder with signing in on (`trunk-pro access add luke`,
`trunk-pro access add kiosk --access listen`, `trunk-pro access on`),
started from a terminal so its log is in view. "Expect" is what passing looks
like. Each says why it isn't automated.

### Passkeys on real authenticators

1. **Touch ID, Chrome on macOS.** Open `http://localhost:8080`, sign in with
   luke's token, top bar → luke → Add passkey → Add; Touch ID. Sign out.
   Expect: the sign-in page's token field offers the passkey in its
   autofill (conditional UI); picking it signs in with one touch; Setup →
   Access lists "Mac Chrome". *Why manual: platform authenticators need a
   finger and the OS keychain; the virtual authenticator stands in for both.*
2. **Touch ID, Safari on macOS.** As 1 in Safari. Expect: Safari's own
   sheet; the passkey lands in iCloud Keychain (System Settings → Passwords
   shows it for `localhost`); autofill offers it. Then in Chrome, sign in
   with the same passkey (Chrome offers iCloud Keychain passkeys on recent
   macOS). Expect: signs in; the config's counter stays 0 (synced passkeys
   count nothing). *Why: Safari has no DevTools virtual authenticator;
   iCloud sync is Apple's.*
3. **Firefox on macOS.** As 1. Expect: works; note whether its autofill
   offers the passkey (conditional UI support varies by version). *Why:
   Firefox isn't driven by the CDP test.*
4. **Windows Hello, Edge or Chrome.** Recorder on a Windows PC (or reach it
   through `server.hostname` over HTTPS), add with Hello PIN/face, sign out,
   sign in. Expect: as 1. *Why: needs Windows and Hello.*
5. **A phone by QR (hybrid).** In desktop Chrome on the sign-in page press
   Passkey → "Use a phone or tablet" → scan with an iPhone, then an Android
   phone. Expect: signs in on the desktop; adding a passkey this way stores
   it on the phone. Needs Bluetooth on both. *Why: caBLE/hybrid needs two
   devices and Bluetooth proximity.*
6. **A phone's own browser.** Over `https://<hostname>` (Tailscale cert or a
   tunnel, below), iPhone Safari and Android Chrome: add a passkey from the
   top bar popover (check the popover fits a phone screen), sign out, sign
   in from autofill. *Why: mobile browsers, real HTTPS.*
7. **A hardware key (YubiKey or similar): the counter path.** Add a passkey
   with it (Chrome: "Use a security key"; it must hold a resident key). Sign
   in three times. Expect: the config's `key.cred.counter` rises each time
   (synced passkeys stay at 0). Make the config folder read-only and sign in
   again: expect a 503 "the passkey's count couldn't be saved", no session.
   Remove its passkey in Setup → Access: expect that browser signed out at
   once. *Why: a real counter only a device keeps (a copy counting back is
   refused in the unit test; it can't be made with a real key).*
8. **Password managers.** With 1Password and with Bitwarden extensions in
   Chrome: add a passkey (the extension's prompt replaces the browser's),
   sign in from autofill and from the button. Expect: both work; counter 0.
   *Why: their extensions hook WebAuthn; can't be loaded headless.*
9. **Excluded on the same device.** After 1, press Add passkey again in the
   same browser. Expect: the browser says this device already has one
   ("On this device already"), nothing added. *Why: excludeCredentials
   handling differs per authenticator.*
10. **Wrong origin, by hand.** With the dashboard reached through an SSH
    tunnel on another port (`ssh -L 9000:localhost:8080 …`, open
    `http://localhost:9000`): expect no Passkey button on its sign-in page,
    a link to `http://localhost:8080/signin` only if that answers on this
    computer, and the token still works. *Why: real tunnel.*

### Certificates and HTTPS

11. **Tailscale cert.** `tailscale cert radio.<tailnet>.ts.net`, set
    `server.tls` to the two files, `server.hostname` to that name, bind
    `0.0.0.0`, restart. From another tailnet device open
    `https://radio.<tailnet>.ts.net:8080`. Expect: a valid padlock; sign-in
    cookie `Secure`; passkeys offered at that origin; `http://…:8080/` gets
    a 308 to https; `curl http://…:8080/api/v1/calls -H "Authorization:
    Bearer …"` gets the 400 "sent unencrypted" and the log says so once.
    *Why: a real CA-issued cert and a second device.*
12. **Let's Encrypt.** As 11 with certbot's `fullchain.pem`/`privkey.pem`.
    Expect the same. *Why: real ACME.*
13. **Renewal without a restart.** Renew (`tailscale cert` again, or `certbot
    renew --force-renewal`). Within a minute of the files changing, expect
    the log line "read again (renewed)" and the browser seeing the new
    certificate's dates (padlock → certificate). Then write a broken key
    file in place: expect one warning, the old certificate still served.
    *Why: real renewal tools rewrite files their own way.*
14. **HSTS.** Set `"hsts": true`, load the dashboard once over https, then
    type `http://<host>:8080`. Expect: the browser goes to https itself (no
    request reaches the recorder over plain HTTP: its log shows none).
    Then check another plain-HTTP service on the same host name (any port)
    is now forced to https too, as the docs warn. Clear it afterwards
    (Chrome: `chrome://net-internals/#hsts` → Delete). *Why: HSTS is browser
    state.*

### Cloudflare Tunnel and Access

15. **Tunnel alone.** `cloudflared tunnel` to `http://localhost:8080`,
    `server.hostname` the public name. Expect: the sign-in page at
    `https://radio.example.com`, the cookie `Secure`, passkeys at that
    origin (no port). Without `server.hostname`: expect 403 "only answers to
    localhost". *Why: a real tunnel.*
16. **Access in front.** Add an Access application with email login; put its
    team name and AUD tag in Setup → Access → Proxy → Cloudflare; map your
    email to luke. Expect: arriving through Access you are luke with no
    token, `/api/session` says `"proxy": true`, no Sign out button (it's
    Access's). Check the token's `iss` is `https://<team>.cloudflareaccess.com`
    and `aud` contains the tag (decode `Cf-Access-Jwt-Assertion` from the
    browser's request, or the `CF_Authorization` cookie, at jwt.io).
    *Why: real Cloudflare keys and tokens.*
17. **A wrong AUD.** Change one character of the AUD tag and save. Expect:
    signed out at once; the log says "is for another application" once.
    *Why: as 16.*
18. **Token expiry.** Set the application's session duration to 15 minutes;
    leave the dashboard open. Expect: within ~16 minutes of signing in, its
    socket closes and the page goes through Access again. *Why: real time.*
19. **Removing someone at Cloudflare.** Remove a user's email from the
    Access policy (and revoke their sessions in Zero Trust → Users). Expect:
    their next request is stopped by Access; their open dashboard socket
    keeps going until the token it came in with expires (up to the session
    duration) — note how long. *Why: Cloudflare's side.*
20. **Key rotation.** Zero Trust → Settings → Authentication → rotate the
    signing key (or wait for Cloudflare's). Expect: sign-ins carry on (the
    log may show the keys fetched again); nothing about unknown keys more
    than once a minute. *Why: real rotation.*
21. **A service token.** Call through Access with a service token
    (`CF-Access-Client-Id/Secret`). Expect: no sign-in (it names no email);
    the log says "names no email" once; an access token in `Authorization`
    still works. *Why: real Access.*

### Other proxies

22. **Authelia or Authentik (forward auth), with Caddy or Traefik.** Proxy
    on the same machine, `proxy.from: ["127.0.0.1"]`, header `Remote-User`.
    Expect: signed in as the proxy's user; mapped names honoured; others get
    `other`'s access. Then send `Remote-User: luke` directly to the
    recorder's port from another computer (recorder bound to 0.0.0.0).
    Expect: ignored, and the log names that address once. *Why: real
    proxies set headers their own way (and must strip incoming ones).*
23. **The proxy strips the header.** Through the proxy, add
    `-H "Remote-User: luke"` as an unauthenticated user. Expect: the proxy
    removes or overwrites it (if it doesn't, that's the proxy's
    misconfiguration, and the recorder can't tell). *Why: proxy behaviour.*
24. **oauth2-proxy.** Header `X-Forwarded-User` (or `X-Forwarded-Email` with
    `users` mapping). Expect as 22. *Why: as 22.*
25. **`tailscale serve`.** `tailscale serve --bg 8080`, header
    `Tailscale-User-Login`, `from: ["127.0.0.1", "::1"]`, `hostname` the
    `ts.net` name. Expect: tailnet users signed in by login; a tagged device
    (no user header) gets anonymous access. *Why: real Tailscale.*

### Ports, containers, cookies across ports

26. **Docker port mapping.** `docker compose up` with `-p 8090:8080`. Open
    `http://localhost:8090`. Expect: sign-in works with a token; no Passkey
    button (the recorder's own port is 8080 inside); the docs' note about
    `localhost` passkeys applies. *Why: real Docker networking (peer isn't
    loopback).*
27. **Cookies across ports, each browser.** Two recorders on one machine
    (`--port 8080` and `--port 8081`, separate `--config` folders), plus an
    interface on 8082. In Chrome, Safari and Firefox: sign in on 8080 only.
    Expect: 8081 is still signed out (its cookie has another name); the
    interface's page on 8082 can't use 8080's session (its fetches are
    same-site: `/api/session` from that page says `user: null`), but a link
    from 8082 to `http://localhost:8080` arrives signed in. Safari lacks
    `Sec-Fetch-*` on older versions: note whether 8082's requests to 8080
    send them (if not, they count as a program's). *Why: per-browser cookie
    and fetch-metadata behaviour.*
28. **Signed out everywhere, two browsers.** Sign in as luke in Chrome and
    Safari; Sign out everywhere from Chrome's top bar. Expect: Safari's open
    dashboard drops to the sign-in page within a second or two. *Why: two
    real browsers.*
29. **Mobile browsers, View user.** On a phone, sign in as kiosk. Expect: no
    Setup, Start or editors; live audio plays; the top bar's name opens the
    passkey popover. *Why: real mobile layout and audio.*
