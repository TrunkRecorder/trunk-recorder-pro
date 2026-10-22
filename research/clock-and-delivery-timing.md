# Clock health and call delivery timing (#61)

Issue #61 asks for two things on the dashboard: how well the host's clock is
synchronized, and how long calls take to reach Broadcastify Calls. This is
the design, from a survey of the code at `origin/main` on 2026-10-22. Tick an
item when it's merged. Strike out or annotate an item that's decided
against, with a word on why.

**Verdict.** Both halves belong in the recorder, not in the Broadcastify
plugin. Neither needs a new dependency. Most of the pieces are already
there:
- a periodic network-check thread;
- the stats store, with its history and rolling windows;
- a `CallResult` for each call, matched to the call by its path;
- the delay rules' intended wait for each plugin;
- the P25 time broadcast, already decoded.

What's missing is one clock component, and a timing measurement for each
call each plugin delivers.

## What's there now

**Host metrics.** `trunk-pro/src/platform.rs` measures CPU, memory, disks,
temperature and network. `Probes` connects to each target over TCP every
`PROBE_EVERY` (15 s) on a thread of its own. It reports `net/up`, `net/rtt`
and `net/dns` to the stats sink, plus up/down transitions and the
Platform page's rows. Nothing looks at the clock.

**Plugin results.** Plugins send `CallResult { path, outcome, message, url }`
for each call. `plugins/manage.rs` (`notes`) counts them (ok, failed,
skipped) and passes the path to the archive (`archive.result`). No time is
recorded.

**Upload latency.** The SDK's `CallQueue` (`queue.rs`, around `took_ms`)
times each upload. Its `Metrics` and each `Endpoint` report a smoothed
`latency_ms`, but per lane only, not per call.

**Delays.** `plugins/hold.rs` `Holds::delay(label, about)` knows how long a
plugin's delay rules hold a call. Delivery timing has to subtract this, or a
15-minute delay rule looks like a 15-minute upload.

**Broadcastify.** The upload answer is `0 <url>`, `1 SKIPPED…`,
`1 REJECTED…` or an error (`trunk-plugin-broadcastify/src/upload.rs`,
`answer`). The API reports no time skew, so "server-reported skew" cannot be
shown. The interface says so rather than leaving a blank, and no local
estimate is ever labelled as server skew.

**The radio's own clock.** `trunk-core/src/p25/osp.rs` decodes SYNC_BCST
(`Msg::SyncBroadcast`). `sync_time` keeps year to minute and drops the
13-bit micro-slot count (7.5 ms each, 8000 a minute). Phase 2 systems keep
that time GPS-disciplined. `trunk/message.rs` only logs it.

**Sample time vs wall time.** `trunk-app/src/session.rs` `ClockFit` anchors
sample time to wall time. It re-anchors when the offset jumps by more than
`REANCHOR_S` (0.25 s). That covers a time service stepping the clock, but
the step isn't recognized as a clock event.

## Duplication found

- Seven hand-written wall-clock helpers:
  - `unix_now` in `platform.rs` (f64), `server.rs` (i64) and the SDK's
    `queue.rs`;
  - `now_ms` in `plugins/hold.rs`;
  - `now` in `store.rs` and `plugins/manage.rs`;
  - plus 24 direct uses of `UNIX_EPOCH` across `runtime.rs`, `monitor.rs`,
    `logging.rs`, `store.rs`, `hold.rs`, `cli.rs` and the SDK's
    `multipart.rs`.
- `Probes` is TCP-only: the scheduling, transitions and reporting are tied
  to `TcpStream::connect_timeout`, so a clock check would need a second
  thread and a second reporting path.
- Rolling statistics: the issue asks for 5-minute, 1-hour and maximum
  latency. The stats store's `History` already keeps bounded series with
  those windows, so no new rolling structure is needed.

## Design

### 1. One `clock` module (trunk-app)

- `clock::unix_s()` and `clock::unix_ms()` replace the seven helpers and the
  scattered `UNIX_EPOCH` uses in the app. The SDK keeps its own, since it's
  a separate published crate.
- `clock::Health { synced, est_error_ms, offset_ms, measured_at, source }`
  is the latest view of the clock, readable from anywhere. A call stamped
  while the clock is known to be unsynced can say so in its JSON. That
  matters when calls from several recorders are merged (multi-site
  de-duplication, the directory).

### 2. Measuring the host clock, on every platform, no new dependencies

- **What the OS says: `ntp_adjtime` / `adjtimex`.**
  - `libc` is already a Unix dependency.
  - It gives the sync state (`STA_UNSYNC`, `TIME_ERROR`), the estimated and
    maximum error, and the offset.
  - It works the same under chrony, systemd-timesyncd and ntpd: no
    per-service code, no subprocess, no privileges.
  - In a container it reads the host kernel's clock, which the container
    shares.
  - Linux certainly; macOS's `ntp_adjtime` is to be confirmed in the `libc`
    crate.
- **Our own measurement: SNTP.**
  - One 48-byte UDP exchange (`std::net::UdpSocket`) every 10–15 minutes to
    a configured server (default `pool.ntp.org`).
  - The standard four-timestamp formula gives offset and round-trip delay.
  - Works on every platform, Windows included, and in containers.
  - It only measures and never sets the clock, so it isn't a second NTP
    client.
- **`Probes` becomes a list of `Check`s.** `TcpConnect` and `Sntp` share the
  scheduling, transitions, sink and rows. SNTP reports `clock/offsetMs`,
  `clock/delayMs`; `ntp_adjtime` reports `clock/synced` and `clock/estErrMs`.
- **Later, best-effort:** chrony's reference name and last update (its local
  socket protocol, or `chronyc -c tracking`), and timesyncd's server
  (`timedatectl show-timesync`). They're shown when found, never needed.

### 3. Delivery timing, in the host, for every uploader

At each `CallResult`, the host knows:
- when the call ended (from the call, by its path);
- when it offered the call to the plugin;
- how long the delay rules held it (`Holds::delay`).

The SDK adds `took_ms` to `CallResult`: the upload's own duration, which
`queue.rs` already measures. Then, per plugin (and per copy):

| Measure | Meaning |
|---|---|
| Held | the delay rules' intended wait |
| Queue wait | offered → upload started, less Held |
| Upload | `took_ms` |
| Age at delivery | call ended → result, on the local clock |

Each is a series under `plg/<id>/…` in the stats store, with failures
counted apart. OpenMHz, Rdio Scanner and the upload script get these as
well as Broadcastify, and the Broadcastify plugin needs no change. If
Broadcastify ever reports skew, the plugin sends it in `Metrics.extra`.

### 4. Dashboard

- **Platform page:** a Clock card with synced (the OS), measured offset
  (SNTP, labelled as measured), the reference server, and when it was last
  measured. Charted over time.
- **`platformHealth`:** warns when the OS says the clock is unsynced, or the
  measured offset is past a threshold (250 ms to start, configurable in
  phase 3). Changes are monitor events, as network up/down changes are.
- **Each uploader plugin's page:** Held, Queue wait, Upload and Age at
  delivery, with latest, 5-minute, 1-hour and maximum values from the
  series' history. A note says that age at delivery is local and that
  Broadcastify reports no skew.

### 5. Later: the radio system's clock

Keep SYNC_BCST's micro-slots, and time the message by its samples through
`ClockFit`. Phase 2 P25 systems then become a GPS-disciplined reference that
needs no internet, compared with the host clock. `ClockFit`'s re-anchor
would then be reported as a clock step.

## Plan

### Phase 1: host clock
- [ ] `clock` module: `unix_s`, `unix_ms`, `Health`; replace the helpers and
      `UNIX_EPOCH` uses in trunk-pro and trunk-app
- [ ] `Check` trait: `Probes` over `TcpConnect` and `Sntp`
- [ ] `ntp_adjtime` reading (Linux; macOS if `libc` has it), "unavailable"
      elsewhere
- [ ] `clock/*` series; Clock card on the Platform page; `platformHealth`
      warning; monitor events
- [ ] Docs: what each number means, supported platforms, limits
      (containers, Windows)
- [ ] Tests: the SNTP packet and offset math; the adjtime status mapping;
      a render test for the Clock card

### Phase 2: delivery timing
- [ ] SDK: `CallResult.took_ms` from `CallQueue`; publish; re-release the
      plugins
- [ ] Host: offer time per call and plugin; Held from `Holds::delay`; the
      four measures as `plg/<id>/…` series
- [ ] Plugin page: the four measures with their windows; the note on skew
- [ ] Tests: a held call's age less its hold; a failed call apart; a plugin
      on an older SDK (no `took_ms`) shows the age only

### Phase 3: optional
- [ ] Configurable clock offset threshold
- [ ] SYNC_BCST micro-slots and timing; the radio-clock comparison; clock
      steps as events
- [ ] chrony / timesyncd details where found

## Not doing

- Running an NTP client that sets the clock.
- Asking Broadcastify for anything beyond the uploads it already gets.
- Keeping per-call timing beyond the stats store's bounded history.
