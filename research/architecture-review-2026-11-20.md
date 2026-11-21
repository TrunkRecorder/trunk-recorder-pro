# Architecture review, 2026-11-20

Reviewed at main `3c810ae` (v0.3.9), 618 commits after the last review
(`architecture-review-2026-10-17.md`). Eleven read-only reviewers, one per
layer plus one tracing a call, a talkgroup, system identity, time and
frequency end to end. Nothing was built, run or changed.

How to read the marks:

- **V**: I read the cited code myself and the claim holds.
- **R**: a reviewer reported it; not re-checked. Confirm before fixing.
- Open items of the October tracker are not repeated here unless they got worse.

## Verdict

The spine is sound: one `CallRecord` from engine to file, store, plugins and
browser; one SQL layer; system ids in the store; integer Hz everywhere; the
lifecycle enum and engine command channel; Rust and TypeScript field names
agree. The growth since October went into the edges, and the edges show five
repeating problems:

1. **Identity by name.** The store has system ids, but the run, the plugin
   host, delays, names, stats and band plans still key by short name or a
   label. Every rename, name collision and re-use bug below comes from this.
2. **Parallel pipelines that drift.** Trunked vs conventional, Phase 1 vs
   Phase 2, live vs replay, SetConfig vs edit_config. Each pair does the same
   job twice, and a fix on one side has not reached the other.
3. **Protocol knowledge is scattered and stringly typed.** A new protocol
   touches about 40 files, the compiler guides about a third, and the rest
   default silently to P25.
4. **Policy gates with side doors.** `shareDecrypted`, delays, Ignore and the
   key check each have one path that skips them.
5. **Legacy shims in a no-legacy beta.** `migrate.rs` runs on every config
   read, and one of its shims causes a wrong-system bug.

## 1. Bugs

### Data goes to the wrong place or is lost

- [x] **V** (fixed on `review-step1`: `name_problem` rejects the clash) A site named like another system's short name files that system's
  calls under the wrong id. `Db::set_systems` adds every site name
  (`migrate::old_names`) to the system name map with `or_insert`, and
  `name_problem` does not reject the clash. `trunk-pro/src/db/mod.rs:299-321`,
  `trunk-app/src/migrate.rs:421`, `trunk-app/src/config.rs:2625-2644`.
  Fix: delete `old_names` (no-legacy), or reject the clash.
- [x] **V** (fixed on `review-step2`: the run files calls under its config ids) Renaming a system while recording stops its calls reaching the
  store. A config save swaps the name map at once; the engine keeps the old
  name until Stop/Start; `Db::send` drops a call whose name has no id, at
  debug level. Backfill later walks only current names.
  `trunk-pro/src/runtime.rs:235-237`, `db/mod.rs:286-291`.
  Fix: resolve the system id at Start and carry it on the run.
- [x] **V** (fixed on `review-step2`: `About` and delay rules by `SystemId`) The same rename releases delayed plugin calls. Delay rules are
  rebuilt under the new name; held and new calls carry the old one, so a
  system-scoped rule no longer picks them. `plugins/delay.rs:85,119-126`,
  `server.rs:924`. Fix: key `About` and rules by system id.
- [x] **R** (fixed on `review-step2`: `NameLists`, `NameBook` and the registry by `SystemId`) After a rename, `ctx.names` stays keyed by the old name for the
  life of the process: Stop/Start records with no talkgroup rows, and one
  unit-name edit then overwrites the stored list.
  `trunk-pro/src/runtime.rs:101,531,735-738`.
- [x] **R** (fixed on `review-step2`: names, aliases and registry deleted by id; dashboard history and the live list still go by name) Data → Delete on a removed system empties a current system of the
  same name (the name-keyed steps run after the id lookup).
  `trunk-pro/src/data.rs:247-284`.
- [ ] **V** A failed store batch is only logged; urgent name edits vanish.
  `trunk-pro/src/db/writer.rs:129-143`.
- [ ] **V** A failed WAV or JSON write drops the call with one warning: no
  row, no browser entry, no plugin delivery, no monitor event.
  `trunk-pro/src/runtime.rs:1266-1272`.
- [ ] **V** Backfill reads the deleted-before markers once, so a delete during
  a walk is undone. `trunk-pro/src/db/backfill.rs:202-204`.
- [ ] **V** IQ lookback loses the blocks that arrive between the last ring
  drain and arming, with no gap marker. `trunk-pro/src/iq_capture.rs:239-263`.
- [ ] **V** The lookback ring keeps old-centre samples after a retune when no
  job is running. `iq_capture.rs:213-219`.

### Policy applied on one path only

- [x] **V** (fixed on `review-step1`) A conventional call is not gated again when the air names its
  talkgroup: an Ignore talkgroup is recorded and uploaded, and a skipped start
  stays skipped. Trunked re-runs `admit`.
  `trunk-core/src/trunk/conventional.rs:1376-1384,1401,1424-1425` vs
  `calls.rs:630-634`.
- [x] **V** (fixed on `review-step1`: `Talkgroup::named_by_row`, used by the engine and `NameBook`) A named conventional row defeats the talkgroup file's Ignore,
  priority and mode: the engine takes the row's info (Ignore off) whole, while
  the dashboard's `NameBook::tg` keeps the file's Ignore.
  `conventional.rs:1486-1488`, `trunk-app/src/config.rs:1374-1393`,
  `stats/mod.rs:127-133`. Fix both with one merge helper and `Call::retalk`.
- [ ] **V** Phase 2 has no key check: a stale key records noise as decrypted,
  with no WrongKey note. `trunk/tdma.rs:190-195` vs `tracker.rs:477-493`.
  Not a quick fix, and deliberate: `KeyCheck`'s doc (`p25/crypt.rs:338-347`)
  says half-rate AMBE+2 has no tell (noise hits its erasure and tone codes 1
  time in 30). Needs a different test, or stays a known limit.
- [x] **V** The plugin `log` topic skips the dispatch gate (shareDecrypted and
  delays) (fixed on `review-step1`: `Shared::offer`, lines carry their call's
  talkgroup; `CallsQuery` fixed on `review-step2`: answered through the queue, never blocking, filtered by shareDecrypted and delays), and `CallsQuery` answers every plugin unfiltered with a blocking
  send. `trunk-pro/src/plugins/host.rs:746-757,935-944` vs `:670-691`.
- [ ] **V** A plugin with a config error can settle a call's archive while
  another plugin is still uploading: `result_at` counts down without checking
  the label, and `call_takers` and `dispatch` disagree on who takes a call.
  `plugins/archive.rs:275-290`, `host.rs:477-488`.
- [ ] **V** XPT voice params never reach the decoder: the engine merges only
  `tdma_key` and `nac`. `trunk/engine.rs:1561-1563`, `voice.rs:267-275`.

### Numbers and ids

- [ ] **V** EDACS counters go backwards on a control-channel change and the
  session subtracts unguarded (wraps in release).
  `edacs/site.rs:126-128`, `trunk-app/src/session.rs:1070`.
- [ ] **V** A recorder stays counted until the end of the block, giving a
  false `NoRecorder` at capacity. `engine.rs:1168-1172,2390-2394`.
- [x] **V** (fixed on `review-step2`: live audio, tone patterns and `call_num` carry the conversation id; no separate copy id, as nothing reads one) A multi-site call has three ids: the conversation's (call list,
  `CallInfo`), the leading copy's (live audio, tone patterns) and the kept
  copy's (`call_num`). The Calls panel's now-playing lookup and any plugin
  join by id fail. `multisite.rs:191-193`, `engine.rs:2234,2329`,
  `record.rs:201`.
- [ ] **V** `Source::label()` is not unique yet identifies a source in stats,
  AutoTune and the gain tuner's keep. `config.rs:392-408`,
  `stats/mod.rs:267`, `session.rs:786,801`, `trunk-pro/src/tune.rs:271`.
- [ ] **V** `Core.now_s` follows source 0 only. `engine.rs:2160-2162`.
- [ ] **R** Stats, day roll and monitor events run on wall-at-Start plus
  `Instant`, never corrected, while calls use the corrected epoch and queries
  the real clock. An NTP step or a sleep offsets them until the next Start.
  `runtime.rs:1098,1174`, `session.rs:674`.
- [ ] **V** `clean_voice_pct` is 100 from a record and 0 for an unsaved call
  with no frames. `facts.rs:57`, `session.rs:1398`.

### Config changes and lifecycle

- [x] **V** (fixed on `review-step2`: `server::apply_config`, interfaces served from one notified task) Two config-change pipelines: SetConfig applies logging, unlock,
  share, live sources, interfaces and plugins inline; `edit_config` applies
  only save, delays and plugins. `trunk-pro/src/server.rs:895-954` vs
  `:812-826`. Fix: one `Ctx::apply_config(old, new)`.
- [x] **V** (fixed on `review-step2`: Start reads the config once the run before has stopped) Start clones the config before waiting on the lifecycle lock, so
  an edit made during the stop is neither run nor flagged.
  `server.rs:1067-1070`, `runtime.rs:274`.
- [x] **V** (fixed on `review-step2`: replay sets the keys on every system, its capture's too) Replay replaces the CLI's config with the capture's and never sets
  keys, so `--keys` is ignored. `trunk-pro/src/main.rs:545-549`.

### Web

- [x] **V** (fixed on `review-step1`) Survey "replacing" is offered for sites of any system and sets the
  system's type, turning a DMR, NXDN or EDACS system into P25.
  `web/src/Survey.tsx:530-534`, `web/src/controller.ts:1746`.
- [x] **V** (fixed on `review-step1`: DMR and NXDN channels; EDACS LCNs by each frequency's place in its row, which is how RadioReference keeps them — unverified against a real EDACS export) A sites-file import as DMR, NXDN or EDACS has nothing to watch:
  frequencies land in `voiceChannelsHz`, which those types never read.
  `web/src/directoryImport.tsx:55-94`, `trunk-app/src/config.rs:942-952`.
- [ ] **V** Reconnect drops in-flight whole-list talkgroup and unit-name saves
  and leaves pending `ask()` calls hanging. `controller.ts:361-370`.
- [ ] **V** The Trunk Recorder import applies a stale snapshot (memo deps omit
  the live config). `web/src/Onboarding.tsx:1824-1830`.
- [ ] **R** Frequency text is parsed five ways ("154,325" becomes 154.000 MHz
  or two channels), and survey "add" snaps a stored channel to 6.25 kHz.
  `web/src/config.ts:295,705-712`, `fieldText.ts:60`, `Survey.tsx:96-115`.

## 2. Structure

### Identity

- [ ] (On `review-step2`: the run, plugin host, delays, hold, `NameLists`, `NameBook`, registry and the store by `SystemId`; left by name: stats series, band plans, sharing, capture rules, AutoTune, live listening and the web, which resolve a name against the config at the boundary. Sources not done.) Key the run, plugin host, delays, hold, `NameLists`, stats series and
  band plans by system id, with the short name as a label only. Give sources
  an id for the same reason. This closes six of the bugs above at once.
- [x] (On `review-step2`, without the copy id field) Stamp one call id on audio, patterns and `call_num`; keep the copy id as
  its own field.
- [ ] **R** Talkgroup names freeze at call start, unit names resolve at save.

### One pipeline per job

- [x] **R** (fixed on `review-step2`: `trunk::voice::Crypt`, the key check the one deliberate difference) `VoiceTracker` and `TdmaTracker::Slot` are two copies of one
  crypto and transmission state machine, already drifted (key check, Info
  cadence, protected LC, end clears talker). Share one `p25::CryptState`.
- [ ] **R** Encryption state is mirrored through about six stores with
  one-way latches. One per-transmission `VoiceCrypt` value. (Looked at on `review-step2`, left: the call's flags are what the gates read, the per-transmission ones (`Transmissions`) already are the one value per transmission, and folding them would rework fail-closed logic with no bug to show for it.)
- [x] **R** (on `review-step2`: one `Call::begin` for a grant's call and a conventional one, one gate (`CallManager::admit`, which now also stops recording a call that no longer passes); conventional already shared the manager's note, length and transmission rules. Its carrier-driven lifecycle stays its own: calls start with voice, rows and tones route them. `VoiceParams` are left out on purpose: a conventional P25 channel matches several NACs to its rows, and conventional DMR has no site variant.) Conventional writes voice output, end of copy, gating and naming
  again beside the trunked path, and never passes `VoiceParams` to decoders
  (`conventional.rs:1144`).
- [x] **R** (fixed on `review-step2`: replay is a Session over file sources, its config the capture's or one made from the flags; same call files byte for byte on the regress captures, ~2.5% more instructions, the session's own work) `replay` is a second engine host that bypasses Session
  (`main.rs:397-739`); regress, the CPU gate and the corpus all test it, not
  the path production runs. Make replay a headless Session over file sources.
- [ ] **R** Plugin `CallInfo` and browser `call_view` are two hand-built
  projections of a call (`session.rs:1093-1127`, `:1548-1576`).
- [ ] **R** Survey keeps its own decoder stack with different thresholds; two
  gain tuners. A `survey::Prober` per protocol.
- [ ] **R** Small repeats: `short_name_of` beside `call_systems`
  (`config.rs:2791,2802`); the numbered file-name rule in session and CLI;
  three call lengths; three hand-kept call column lists.

### Protocols

- [x] **R** (on `review-step2`: `WatchedSite` and one `Watched<S>` control channel; each protocol's status stays its own) Watched-site skeleton now has three copies (DMR, NXDN, EDACS). A
  generic `Watched<P>` with one `WatchedStatus`.
- [x] **R** (on `review-step2`, the part the web repeated: `SystemKind::info` (label, watched, site-lock fields) written to `web/src/systemKinds.gen.ts` and checked by a test; fixed EDACS shown as P25 in Onboarding and as NXDN on the guard band page; trunked voice cutoff exhaustive. Voice kinds, capture support and band plan type are still per site of code.) A per-protocol descriptor on `Protocol` (voice kinds, cutoff,
  identity fields, capture support, band plan type), with the TypeScript union
  and a `SYSTEM_KINDS` record generated from it. Replaces `_ =>` defaults
  (`voice.rs:103`, `session.rs:1605-1640`), the hand-kept `VoiceKind::ALL`,
  and the web's P25 fallbacks (`Decode.tsx:442-473`, `Rolloff.tsx:29`).
- [ ] **V** Sitecheck's identity fields have drifted from control's, and both
  take the type as a string. `sitecheck.rs:172-179`, `control.rs:593-661`.
- [ ] **R** "Build a system from facts" is written six times in the web, each
  with different type support.
- [ ] **R** List Viterbi settings are process-global atomics written only by a
  tool (`p25/diversity.rs:522-562`).

### Session, protocol and SDK

- [ ] **R** Session has five dirty-tracking schemes; a `Kept` trait.
- [ ] **R** Browser status is hand-built JSON (about 70 `json!` sites, about
  25 `ToClient` variants carrying `Value`); `protocol_tests` has no sample
  for the newest messages.
- [ ] **R** SDK fields are stringly typed (`SystemInfo.kind`,
  `UnitEvent.kind`, `CallInfo.reason`); `takes_wav` hard-codes
  `"upload-script"`; docs say SDK 0.7.0, the crate is 0.12.0.
- [ ] **R** Capture metadata is untyped JSON with hand-kept key lists
  (`TRIGGER_KEYS` omits `peak_db`); capture rules poll event JSON.
- [ ] **R** `engine.rs` is seven modules in one file; split out radio and site.
- [ ] **R** New radio driver touches about seven places; stream loop copied
  across soapy, uhd and bladerf; config to gain written three times.

### Legacy

- [ ] **V** `migrate.rs` still runs about eight upgrades plus `old_names` and
  `old_bandplan` on every config read. Also `SavedRules` and the
  upload-script shim. Decided 2026-11-20: they stay until the next major
  commit, then go entirely. Until then the site-name collision needs its own
  fix (`name_problem` rejects the clash).

### Tests and CI

- [ ] **V** The corpus workflow's path filter omits `crates/trunk-pro/**`, and
  the job is gated on `vars.CORPUS_READY`. `.github/workflows/corpus.yml:10`.
  Decided 2026-11-20: run it nightly instead of widening the filter.
- [ ] **R** Corpus baselines hash scraped stdout; "changed" and "no-baseline"
  pass CI.
- [ ] **R** No end-to-end test over runtime → store → plugins → server.

## 3. Leave alone

Judged clean by the reviewers: the store core (one schema, Exec layer,
`CallRow` guards, Postgres in CI), `multisite.rs`, `calls.rs`, `record.rs`,
`tally.rs`, `wall.rs`, the gain and tune split, `saving.rs`, `share.rs`, the
lifecycle enum, the engine command channel, access checks, `api_v1`, the web
controller reducer, the wasm rules and the dashboard key helpers.

## 4. Suggested order

1. Quick, high-value fixes: reject a site name equal to another system's
   short name; conventional re-gate and name merge; Phase 2 key check; `log`
   topic gate; the two web type bugs; corpus on a nightly schedule.
2. System id through the run (rename bugs, delays, names, stats keys), then
   one call id for multi-site. With it, one send function for plugins: every
   line through the gate, non-blocking, and `CallsQuery` answers filtered row
   by row (added 2026-11-20).
3. One `apply_config`, and replay as a headless Session.
4. Shared P25 crypt state and `VoiceCrypt`; `Watched<P>`; the protocol
   descriptor with generated web types.
5. Conventional onto the trunked call path: conventional reports voice heard
   on a frequency for a talkgroup, `calls.rs` admits, re-gates, names and ends
   it; `VoiceParams` reach conventional decoders (added 2026-11-20).

## Open questions

- Hold deadlines are wall-clock: a forward clock step releases delayed calls
  early. Acceptable for a privacy delay?
- Conventional's synthetic talkgroup numbers (`kHz*10+k`) can collide with
  real ones in stats and plugin filters.
- A plugin `UnitEvent` goes out for `source >= 0`, calls need `> 0`. Do
  unit-0 events reach plugins?
- In-app replay stamps calls from today's clock, not the capture's.
