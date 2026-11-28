# Call products: what plugins work out from a call

Started 2026-11-28, branch `call-products`.

## The gap

A plugin can be given a call (its files at `call.concluded`, its audio, its
taps) and can say what became of it (`call.result`: uploaded, skipped,
failed, a link). It can't give anything back. Two kinds of plugin now need
to:

- **Data decoders.** With `fm.discriminator` a plugin can decode POCSAG and
  FLEX; with an AM tap, ACARS. What they decode is the point of the call:
  the audio is a modem noise nobody wants, the rest of the call (when,
  which channel, how strong, which source) is worth keeping.
- **Transcribers.** A plugin sends the audio to a speech model and gets
  text back, seconds or minutes after the call was saved, and more than one
  may run on the same call.

Both make *something worked out from a call*. One mechanism for both, and
for what comes later (APRS, AIS, DTMF strings, summaries, translations).

## The design

A **product** is what a plugin made of a call: its kind, who made it, the
call it is of, and the thing itself.

```rust
PluginMessage::CallProduct(Product)        // "call.product"

pub struct Product {
    pub transmission_id: String,   // the call it's of (see "Which call")
    pub call_id: Option<u32>,      // … or the running call's, from a live decoder
    pub by: String,                // the plugin that made it (the recorder fills it in)
    pub time: f64,                 // when it was heard or made, epoch s
    pub data: ProductData,         // { "kind": "page", "data": { … } }
}

pub enum ProductData {
    Transcript(Transcript),   // language, text, segments [{start_ms, end_ms, unit, text, confidence}]
    Page(Page),               // protocol, baud, address (capcode), function, alpha/numeric/tone, text
    Acars(AcarsMessage),      // mode, registration, flight, label, block id, ack, message no, text
    Other { kind, data },     // a kind this build has no type for: kept and shown as its JSON
}
```

Every kind has a plain `text()`: what is searched and what a list shows.
The known kinds are shared types in the SDK, so the recorder, the web
interface and every plugin read one definition; a kind the recorder doesn't
know is still kept, searched by its `text` field and passed on, so a plugin
can make a new kind without waiting on a recorder release.

### Which call

A product is anchored on the call's **transmission id**
(`<system>-<talkgroup>-<radio>-<start ms>`): it's in the call's JSON, it's
the same for every site's copy of a call, and it means the same thing after
the database is rebuilt or the files are moved to another machine.

A transmission id is only made when the call concludes, and a live decoder
is working before that: a `TapChunk` names the running call by `call_id`.
So a plugin says whichever it has:

- after `call.concluded` (a transcriber): `transmission_id`, from the
  `CallRecord` it was handed;
- while the call runs (a pager decoder): `call_id`, from the tap chunk.

The recorder turns a `call_id` into the transmission id when the call
concludes (`CallRecord::call_num` is that id) and holds the product until
then. What is stored and sent on always has the transmission id. A product
for a call that was never saved (too short, a duplicate, filtered) goes
with it.

### Where it's kept

1. **The store**: a `call_products` table (transmission id, kind, by, time,
   text, data as JSON), found by transmission id, with the text searched
   (FTS5 in SQLite, `tsvector` in PostgreSQL). The database is always
   there, so this is the one place every product is.
2. **Beside the call**: `<call>.products.jsonl`, one product a line,
   appended. Written when the call's JSON is kept (`callLog`), by the same
   rule and removed with it, so a folder of calls carries its transcripts
   and pages with it and the store can be rebuilt from the folder
   (backfill reads it).
3. **Not the call's JSON.** That is written once at conclusion and is what
   uploaders send; a transcript arriving a minute later would mean writing
   it again under them. Audio tags are untouched for the same reason.

Several products of a call are normal: two transcribers, or a pager
transmission with thirty pages. Nothing replaces anything; `by` and `kind`
tell them apart. A plugin that improves on its own product (a second pass)
sends another, and readers take the latest of a kind from a plugin.

### Who hears of it

- **Plugins**: topic `product.<kind>` (`product.page`, `product.transcript`),
  the same `Product` with `by` and the transmission id filled in, sent when
  it's stored. An alerting plugin subscribes to `product.page`; an uploader
  that wants the transcript subscribes to `product.transcript`.
- **Holding an upload for a transcript**: the existing plugin-delay rules
  gain "until product `<kind>`, at most N s", so an uploader's
  `call.concluded` can wait for the transcriber. (Later step.)
- **The interface**: a `product` message on the socket, the call page shows
  its products, the calls list shows the text of data-only calls and
  searches products' text.

### Data-only channels

A conventional row (or a system) can be marked **data only**. Its calls are
calls like any other: the squelch opens, a call starts, it has a frequency,
a source, a signal level, a start and a length, it shows live. But:

- its audio isn't written, encoded or uploaded (`calls.audio` is false, as
  for calls whose WAV wasn't kept), and it isn't sent to plugins as
  `call.concluded` (uploaders have nothing to upload);
- it is kept if a product arrives for it and dropped if none does, as
  `NotSaved::NoData` ("No data"), after a short wait (a few seconds past
  its end) for a decoder to finish. A pager channel that keys up with
  nothing decodable leaves no rows; the drop is counted like every other.

Products of a data-only call are sent on as they arrive rather than at
conclusion where they can be (the call is already saved by the first one),
so a page is on the alerting plugin's topic within a second of being
decoded.

### The AM tap

ACARS is 2400 baud MSK on an AM carrier, so the decoder needs the AM
detector's output. `am.envelope`: the envelope over its running level
(1.0 = full modulation is half scale), before the voice low-pass, the low
cut, the limiter and the squelch tail; 16-bit at 22,050 Hz while the
carrier is up. One `Tap`, one `TAPS` row, one emit in `Am`.

## The decoders

Ports of ArgusRF's own TypeScript decoders (`argusrf/web/src/dsp/`) to
Rust, as plugins, with the decoder cores as plain libraries with tests:

| Plugin | Tap | From |
| --- | --- | --- |
| `trunk-plugin-pager` | `fm.discriminator` | `pocsag.ts`, `bch.ts`, `flexOTA.ts` (the over-the-air FLEX decoder, not `flex.ts`'s simulator codec) |
| `trunk-plugin-acars` | `am.envelope` | `acars.ts` (itself a port of acarsdec's `msk.c` / `acars.c`) |

Notes for the ports:

- The tap gives 22,050 Hz, half scale = 5 kHz deviation. POCSAG (512, 1200,
  2400) and FLEX (1600, 3200 symbols/s) both fit under its 10 kHz low-pass.
  FLEX must point-sample the discriminator (ArgusRF's finding: a boxcar over
  the symbol smears 4-level symbols).
- A tap stream is one transmission, and a pager transmitter may hold its
  carrier for minutes or drop it between batches: a decoder keeps its state
  for a channel across streams only where the stream is continuous
  (`first`), and flushes at `last`.
- A decoder dedupes repeats inside a transmission (as ArgusRF does), not
  across them: every call's products are what was heard in that call.
- ACARS: resample to 12 kHz as `acars.ts` does, so its matched filter and
  PLL constants carry over unchanged.

## Order of work

1. SDK: `Product`, `ProductData`, the three kinds, `call.product`,
   `product.<kind>` topics, `Host::product`, `Plugin::product`;
   `Tap::AmEnvelope`.
2. Core: the `am.envelope` tap.
3. Recorder: the products ledger (call id → transmission id, late
   products), the table, the sidecar, the topic, deletion with the call.
4. Data-only rows and `NotSaved::NoData`.
5. Interface: products on the call page, text in the list, search.
6. `trunk-plugin-pager` (POCSAG, then FLEX), `trunk-plugin-acars`.
7. Plugin-delay rule "until product".
8. Docs: architecture §4.10 and §8, the taps tables, the plugin protocol
   and events pages, a page for data channels.

## As built (2026-11-28)

Steps 1–6 and 8 are done on `call-products`; step 7 (delay "until product") is not.
Where the build differs from the plan above:

- **Data only is a system's recording setting** (`recording.dataOnly`, with the other archive
  rules), not a row's: a pager or ACARS channel is a conventional system of its own.
- **An older call** (not one of the last 2,048 concluded) is found by the path the plugin
  gives with the transmission id (`Product::of_call`), checked against the store.
- **Products ride in the call's entry** (`StoredCall.products`): one more query per page of
  calls, no separate message to ask for them.
- **The store** went to version 10 by adding the table in place (8 → 10, 9 → 10).
- **Search**: the interface's filter looks in the products of the calls it has. Full-text
  search in the store (FTS5 / `tsvector`) is not built.
- **Backfill** does not read `.products.jsonl` yet, and the sidecar isn't written for a call
  whose JSON is kept only because a plugin failed on it.
- **Data-only products are sent on as they are kept**, which is at conclusion (the wait is in
  `finish_one`), not as they are decoded.
- The plugins are local only: `~/Projects/trunk-plugin-pager` and `~/Projects/trunk-plugin-acars`,
  on the SDK by path. FLEX and ACARS are proven on synthetic signals only (no capture here);
  POCSAG locks on argusrf's 152 MHz capture, which holds only idle codewords.

## Open

- A transcript for a call whose audio was later deleted by retention: the
  product goes with the call (it's the call's), which is what's planned.
  Keeping text longer than audio would be a setting of its own.
- Whether a product can carry a file (a decoded image, a long report). Not
  now: `data` is JSON, small. A `files` list beside it can come later
  without changing the rest.
- FLEX on wide (6400) channels needs more than the tap's 10 kHz low-pass
  passes cleanly; measure before promising it.
