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

### 1. One `clock` module (trunk-pro)

In trunk-pro, not trunk-app as first planned: trunk-app is also the browser
build's engine (wasm), where reading the system clock panics. That's why the
engine takes the time from its host (`Session::poll`'s Unix ms).

- `clock::unix_s()`, `clock::unix_ms()` and `clock::unix_s_of(SystemTime)`
  replace the helpers and the scattered `UNIX_EPOCH` uses in trunk-pro. The
  SDK keeps its own, since it's a separate published crate. The few
  `UNIX_EPOCH` uses left compare file times, not the time now.
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
  - Linux `adjtimex` (glibc and musl), macOS `ntp_adjtime`, both in the
    `libc` crate. As read: macOS (timed) status `0x2081`, synced, max error
    297 ms; the Pi testbed (systemd-timesyncd) `0x2001`, synced, max error
    897 ms. The kernel's own offset field is only its correction residual
    (78 µs on the Pi), not the offset from true time, so it isn't shown.
- **Our own measurement: SNTP.**
  - One 48-byte UDP exchange (`std::net::UdpSocket`) every 10–15 minutes to
    a configured server (default `pool.ntp.org`).
  - The standard four-timestamp formula gives offset and round-trip delay.
  - Works on every platform, Windows included, and in containers.
  - It only measures and never sets the clock, so it isn't a second NTP
    client.
- **`Probes` runs a list of `Check`s** (`Check::Connect`, `Check::Time`): one
  thread, each on its own schedule, one record type. The time check doesn't
  count towards `net/up` and raises no link events, because a blocked UDP
  port 123 isn't the internet down. SNTP reports `clock/offsetMs` and
  `clock/delayMs`; the OS reading reports `clock/synced` and `clock/maxErrMs`.
- **One verdict.** `clock::Health::problem` decides what's wrong: warn at
  250 ms, bad at 5 s, warn when the OS says unsynced, and a measurement over
  an hour old counts for nothing. The platform message carries it, and both
  the dashboard light and the `clockOff`/`clockOk` events go by it, so the
  thresholds live in one place.
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
- [x] `clock` module: `unix_s`, `unix_ms`, `unix_s_of`, `Health`; the helpers
      and `UNIX_EPOCH` uses in trunk-pro replaced
- [x] `Check` (an enum, not a trait: two kinds): `Probes` over `Connect` and
      `Time`
- [x] OS reading: Linux `adjtimex`, macOS `ntp_adjtime`; "unavailable"
      elsewhere
- [x] `clock/*` series; Clock card on the Platform page; `platformHealth`
      from the recorder's verdict; `clockOff` / `clockOk` events
- [x] `monitor.timeServer` (default `pool.ntp.org`, `""` off)
- [x] Docs: configuration, the dashboard guide's Clock section, the events
      table
- [x] Tests: NTP timestamps in both eras; offset and delay; answers refused
      (short, another request, kiss-o'-death, unsynchronized); the kernel's
      status; the verdict; a render test for the Clock card and the light;
      a live query (ignored: needs the network), which agrees with macOS's
      `sntp` (35 ms vs 33 ms)
- [ ] A call stamped while the OS says the clock is unsynchronized says so in
      its JSON (moved to phase 2, which touches the call path anyway)

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
