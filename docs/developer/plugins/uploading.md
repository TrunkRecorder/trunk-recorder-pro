# Writing an uploader

Most plugins send calls somewhere: OpenMHz, Broadcastify Calls, Rdio Scanner, a webhook, cloud
storage. They all need the same things: settings for each system, the right audio format, uploads
off the event thread, retries, and honest reporting. This page builds one step by step, with the
SDK's `CallQueue`, `Attempt`, `http` and `health` modules. The basics of a plugin are in
[Writing plugins](../../plugins/writing-plugins.md).

The [OpenMHz plugin](https://github.com/TrunkRecorder/trunk-plugin-openmhz) is a complete example,
about 250 lines with its tests. Read it alongside. Broadcastify, Rdio Scanner and Upload script use
`CallQueue` too.

## 1. Settings

Services usually give each system its own key, so the key goes in `SystemConfig` (a system's sites
share it; see [Settings for each site](../../plugins/writing-plugins.md#settings-for-each-site)
for a site a service knows on its own). The server goes in `Config`, with the real service as the
default:

```rust
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
struct Config {
    /// Upload server
    ///
    /// Leave this as it is, unless you run your own server.
    #[schemars(url)]
    server: String,
}

#[derive(Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase", default)]
struct SystemConfig {
    /// API key
    ///
    /// Leave it empty to not upload this system.
    #[schemars(extend("x-secret" = true))]
    api_key: String,
}
```

An empty key means "don't upload this system". Don't make it an error: most people record more
systems than they upload.

## 2. The manifest

Subscribe to `call.concluded`, and ask for M4A if the service takes it:

```rust
fn manifest() -> Manifest {
    Manifest {
        name: "OpenMHz".into(),
        subscribe: vec![topic::CALL_CONCLUDED.into()],
        audio_formats: vec![format::M4A.into()],
        ..trunk_recorder_plugin::manifest!()
    }
}
```

## 3. Check everything in `start`

Everything that would make every upload fail belongs in `start`, as an `Err` the user can act on:

```rust
fn start(host: Host, setup: Setup<Config, SystemConfig>) -> Result<Self, String> {
    if !setup.has_format(format::M4A) {
        return Err("OpenMHz needs calls as M4A, and there's no M4A encoder on this computer. \
                    Install ffmpeg, then start recording again.".into());
    }
    let mut keys = HashMap::new();
    for s in &setup.systems {
        let Some(c) = &s.config else { continue };
        if !c.api_key.trim().is_empty() {
            host.info(format!("uploading {} (key …{})", s.short_name, last2(&c.api_key)));
            keys.insert(s.id, c.api_key.trim().to_string());
        }
    }
    if keys.is_empty() {
        return Err("Add your API key to the systems you want to upload.".into());
    }
    // …
}
```

A log line per system saying what will be uploaded, and where, saves users a lot of guessing.
**Never log a whole key.** The last two characters are enough to tell keys apart.

## 4. Upload off the event thread, with `CallQueue`

An upload takes from a fraction of a second to a minute, and `call_concluded` has to return in
milliseconds (see [One thread](../../plugins/writing-plugins.md#one-thread-return-quickly)). The
SDK's `CallQueue` does the rest:

- It works calls on background threads.
- It retries failures after 10 s, 1 min, 5 min and 15 min, then gives up. A service that says how
  long to wait (`Retry-After`) isn't tried again sooner.
- It shows a warning status while calls wait to retry, and clears it when they're through.
- It reports each call's result for you, with how long the upload took (`took_ms`).
- At shutdown, it keeps working until the grace period is nearly up. With
  `QueueOptions::saved_in(data_dir)`, it saves calls still waiting to `queue.jsonl` and sends them
  first at the next start.
- It reports its health to the Health tab (`metrics`): how many calls wait, how long an upload
  takes, bytes sent, and whether the service is **up**, **degraded** (retries) or **down** (three
  failures in a row, or five minutes without a success while calls wait). Name the service with
  `endpoint`.

```rust
struct MyUploader {
    queue: CallQueue,
}

// in start:
let opts = QueueOptions { noun: "upload", endpoint: Some("My Service".into()), ..QueueOptions::saved_in(&setup.data_dir) };
let queue = CallQueue::start(host, opts, move |call: &ConcludedCall| {
    // By id: a call saved for the next run still finds its system, renamed or not.
    let Some(key) = keys.get(&call.call.system_id) else {
        return Attempt::Skip("no API key for this system".into());
    };
    if !call.call.has_audio() {
        return Attempt::Skip("no audio".into());
    }
    let Some(m4a) = &call.files.m4a else {
        return Attempt::Fail("this call couldn't be encoded as M4A".into());
    };
    upload(&server, key, &call.call, m4a)
});
Ok(MyUploader { queue })

fn call_concluded(&mut self, call: ConcludedCall) {
    self.queue.push(call);
}

fn shutdown(&mut self, grace: Duration) {
    self.queue.shutdown(grace);
}
```

The work function runs on the queue's threads, so it has to be `Send + Sync`. Move what it needs
(keys, an HTTP client) into the closure.

`QueueOptions`:

| Field | Default | |
|---|---|---|
| `retry_after` | 10 s, 1 min, 5 min, 15 min | How long to wait before each retry; after the last, the call fails |
| `threads` | 2 | Calls worked on at once, in each lane |
| `save_to` | none | Where to save calls still waiting at shutdown; none: they fail. `saved_in(data_dir)` sets it to `queue.jsonl` there |
| `capacity` | 10 000 | Calls waiting beyond this, in a lane, are refused (reported as failed) |
| `noun` | `"call"` | For the status line: `"upload"` makes "3 uploads waiting to retry" |
| `endpoint` | none | The service's name on the Health tab (`My Service at <lane>` for a lane); none: the plugin's name, or the lane's |

`queue.len()` is the calls queued or being worked on.

### One queue for each server

When you let a system set the server again for itself (`system_overrides`; see
[Settings a system overrides](../../plugins/writing-plugins.md#settings-a-system-overrides)),
different systems can upload to different servers. Read the server from `s.settings` for each
system. Then start the queue with `start_by`, which also takes a closure naming the server each
call goes to. Each server becomes a lane with its own workers, retries and up/down status, so a
server that's down doesn't hold up uploads to the others:

```rust
let queue = CallQueue::start_by(host, opts, move |call| lanes.get(&call.call.system_id).cloned().unwrap_or_default(), work);
```

Calls saved by the last shutdown are put in the lane the closure names for them now.

### A call for several places

With [settings for each site](../../plugins/writing-plugins.md#settings-for-each-site), one call
can go to more than one account: `system.targets(&call.call)` lists them. Try each, and make one
outcome of the tries with `Attempt::together`: tried again if any should be (waiting the longest
any service asked), else failed if any failed, else done if any took it, else skipped.

```rust
// In the work function, with the setup's systems moved into it:
let Some(system) = systems.iter().find(|s| s.id == call.call.system_id) else {
    return Attempt::Skip("not a system this plugin is set up for".into());
};
let tries = system.targets(&call.call).into_iter().map(|t| upload(&server, &t.config.api_key, &call.call, m4a));
Attempt::together(tries)
```

With no targets (no settings for any site that heard it), `together` is a skip. A retry tries
every place again, so a place that took the call the first time has to take it again harmlessly,
or your plugin has to remember which places it sent it to.

### Without the queue

A plugin that doesn't use the queue (a streamer, say) can report the same itself with
`host.metrics(&Metrics { … })` when something changes, at most every few seconds: `queued`,
`latency_ms`, `bytes_sent`, `endpoints` (each with its state), and figures of its own in `extra`,
shown as they are. Every field is in [Protocol](protocol.md#metrics). Send `call.result` for each
call with `host.call_result_took`, so the Health tab can tell your work from the waiting.

## 5. Say what happened: `Attempt`

Your upload function decides what each outcome means:

| Return | When | Then |
|---|---|---|
| `Attempt::Done { url }` | It worked. `url` links to the call on the service, if there's one (else `String::new()`) | Reported `ok` |
| `Attempt::Skip(why)` | Not for this plugin: a system without a key, a talkgroup the service ignores | Reported `skipped` |
| `Attempt::Declined(why)` | The service answered and turned it away, as it means to: it has the call already (another recorder on the system sent it), or doesn't take that kind | Reported `skipped`; the service counts as answering (**up**) |
| `Attempt::Retry(why)` | It might work later: no network, a timeout, a 5xx | Retried; `failed` after the last retry |
| `Attempt::Wait(why, wait)` | The service is busy and said how long to wait (a 429 or 503 with `Retry-After`) | Retried no sooner than `wait` (at most an hour) |
| `Attempt::Fail(why)` | It won't work: a wrong key, an unknown system, a file it refuses | Reported `failed`, now |

Getting this right is most of what makes an uploader good. Retrying a wrong key an hour later
helps nobody. Failing a call because of a moment's network trouble loses it.

`Attempt::from_status(status, retry_after, why)` reads an HTTP answer's status the way every plugin
should, as the `http` module's `Failure::from_status` does:

| Status | `Failure` | `Attempt` |
|---|---|---|
| 2xx | none: taken | `Done` (no URL) |
| 429, 503 | `Busy`, with `Retry-After`'s seconds if it gave them | `Wait`, or `Retry` without a wait |
| 408, 5xx, not reached | `Unreachable` | `Retry` |
| 401, 403 | `Refused`: the credentials were refused | `Fail` |
| 404, 405, 410 | `NotThere`: nothing at that address takes it | `Fail` |
| Any other 4xx | `Rejected`: what was sent was wrong | `Fail` |

Use `http::Failure` itself for something other than a call (a health report).

But services often say what's wrong in the response body, not the status code. OpenMHz answers
most refusals with a 500 and a message. Read the service's source or docs, match its messages, and
leave the rest to `from_status`:

```rust
fn outcome(status: u16, retry_after: Option<&str>, text: &str) -> Attempt {
    if status == 200 {
        return Attempt::Done { url: String::new() };
    }
    if text.contains("API Keys do not match") {
        return Attempt::Fail("OpenMHz refused the API key".into());
    }
    if text.contains("Talkgroup does not exist") {
        return Attempt::Skip("OpenMHz ignores talkgroups it doesn't know".into());
    }
    Attempt::from_status(status, retry_after, format!("HTTP {status}: {}", text.trim()))
}
```

Keep the explanation short and in the user's terms: it's what they see next to the call.

## 6. The request

Use an HTTP client with Rust TLS, so the release builds cross-compile. [`ureq`](https://docs.rs/ureq/3)
is small and blocking, which suits `CallQueue`'s threads:

```toml
[dependencies]
ureq = "3"
```

```rust
let agent: ureq::Agent = ureq::Agent::config_builder()
    .timeout_global(Some(Duration::from_secs(60)))
    // So a 500 comes back as a response you can read, not an error.
    .http_status_as_error(false)
    .user_agent(concat!("my-uploader/", env!("CARGO_PKG_VERSION")))
    .build()
    .into();
```

Set a timeout, and keep it well under a minute: a hung upload holds a queue thread and delays
shutdown.

Most upload services take `multipart/form-data`. The SDK's `Multipart` builds the body, for any
client:

```rust
let audio = std::fs::read(m4a)?;
let file_name = m4a.file_name().unwrap().to_string_lossy();
let (body, content_type) = Multipart::new()
    .file("call", &file_name, "application/octet-stream", &audio)
    .text("talkgroup_num", call.talkgroup.to_string())
    .text("start_time", call.start_time.to_string())
    .text("api_key", key)
    .finish();
let resp = agent.post(&url).header("Content-Type", &content_type).send(&body);
```

Send the file under its real name. Services check the extension: OpenMHz refuses anything but
`.m4a` and `.mp3`.

## 7. Match what Trunk Recorder sent

If the service already takes uploads from Trunk Recorder, send exactly the fields it sent,
formatted the same way: the service's code was written against it. The call's JSON (`call.call`)
has Trunk Recorder's field names, and the SDK sums what its uploaders summed: `error_count()`,
`spike_count()`. Name your settings so Trunk Recorder's can be pasted in:
`#[serde(alias = "apiKey")]`.

For a service's talkgroups, the SDK's `Talkgroups` gives each system the recorder's own
**Talkgroups** menu (see [Choosing talkgroups](../../plugins/writing-plugins.md#choosing-talkgroups)).
Trunk Recorder's uploaders took talkgroup allow and deny lists of glob patterns (`"507*"`,
`"12?45"`) instead. The SDK's `TalkgroupFilter` matches them the same way (and ranges,
`"1000-1099"`), and `filter::patterns` reads a list that mixes numbers and strings:

```rust
#[serde(deserialize_with = "trunk_recorder_plugin::filter::patterns")]
#[schemars(with = "Vec<String>")]
talkgroup_allow: Vec<String>,
// …
let filter = TalkgroupFilter::new(&c.talkgroup_allow, &c.talkgroup_deny);
if !filter.passes(call.call.talkgroup) {
    return Attempt::Skip("talkgroup filter".into());
}
```

`TalkgroupFilter::parse` does the same but says which entry isn't a talkgroup, for checking in
`start`.

## 8. Test it without the service

`testing::MockServer` stands in for the service; see
[Testing against a server](../../plugins/writing-plugins.md#testing-against-a-server). The tests
worth having:

- A call uploads, with every field the service reads checked.
- Each refusal the service gives maps to the right `Attempt`.
- A server that's down leaves the call in `queue.jsonl`, not failed.
- `start` refuses no keys, a bad server address, and no M4A (if you need it).
- A system without a key is skipped, with no request made.

Then run it against a stand-in on your computer with `trunk-pro plugin run`, and inside the
recorder, before you release.

## Node health reports

A service that collects calls can also want to know how well each node hears what it uploads.
Subscribe to [`totals`](events.md#totals), and the SDK's `health` module makes the reports, in the
format of the [node health spec](../../api/node-health.md):

- Keep each `totals` message in a `health::History` (`history.push(totals)`).
- Keep a `health::Reporter` for each node you report on: `Reporter::new(select, key)`, where
  `select` is the part of the recorder the node is (`Select::system(short_name)`, one site with
  `Select::site(short_name, site)`, or a system less some sites with `.except(site)`), and `key`
  something of the node's own (its ID), to spread nodes started together.
- On each `totals`, `reporter.due(&totals)` says whether its next report is due. Reports go once an
  interval (5 minutes, unless the service says otherwise), each at a random point in it, so a
  service's nodes don't all report at once, and none in the first minute of recording.
- `reporter.report(&history, node, recorder)` makes it from the totals since its last delivered
  one: `node` is the service's fields naming the node, and `health::Recorder::of(&setup.host)`
  names the recorder as the spec has it.
- Tell it how the send went: `delivered(&report, next_s)` (the service may set the interval) or
  `failed(&totals, failure)` with an `http::Failure`. A failed report's window goes with the next
  (at most 15 minutes of it); a busy service is backed off from, and refused credentials stop the
  reports.

The [Broadcastify plugin](https://github.com/TrunkRecorder/trunk-plugin-broadcastify) sends them.
