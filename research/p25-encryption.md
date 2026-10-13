# P25 decryption: design, progress and what is still unknown

October 2026. Branch `p25-crypt` (worktree `../trunk-recorder-lite-crypt`).
Decrypting encrypted P25 voice, Phase 1 and Phase 2, for users who are
authorized and hold the keys. There is no keyed system here to test on, so
correctness rests on test vectors, synthesized signals, cross-checks
against OP25 and dsd-fme, and reports from users who have keys.

Sources: OP25 (boatbod master: `op25_crypt*.cc`; Trunk Recorder's bundled
snapshot has only ADP, and nothing loads keys into it), dsd-fme (ISC;
`dsd_mbe.c`, `p25p1_ldu2.c`), dsd-neo (GPLv3; keyring, crypto state
machine, `tests/crypto`) and SDRTrunk (no decryption; its `Encryption.java`
is a good ALGID name table). The code here is written from those facts and
the TIA-102 descriptions. OP25 and dsd-neo are GPL and are not ported.

## Progress

- [x] 1. `p25::crypt`: ALGID table, MI LFSR (step, 64-bit advance,
      AES IV expansion), keystreams for ADP / DES-OFB / 3DES / AES-128 /
      AES-256 with the Phase 1 and Phase 2 offsets, and unit tests (FIPS /
      SP 800-38A / RC4 vectors, offset layout)
- [x] 2. Keep ALGID, KID and MI everywhere: tracker state (Phase 1
      current/pending sync, Phase 2 MI from MAC_PTT and ESS-B), a
      `TrackerOut::Cipher` event, `Call.cipher`, the call JSON, the
      `trunk-pro tool` voice dump, and a dashboard "ciphers seen" list per
      talkgroup. (The plugin protocol's `algid`/`keyId` move to item 4,
      together with `decrypted`. The tool shows Phase 2 MIs from MAC_PTT
      only; it doesn't assemble the ESS.)
- [x] 3. Phase 1 decryption plus the keys file and config
- [x] 4. Call flow: tentative admit when the system has keys, a `decrypted`
      flag, decrypted transmissions kept, live listen allowed, and
      `algid`/`keyId`/`decrypted` in the plugin protocol
- [x] 5. `p25::synth` and cross-checks against OP25 master and dsd-fme
- [x] 6. Phase 2 decryption
- [ ] 7. ~~Late entry: decrypt the first superframe by running the LFSR
      backwards~~ (done with item 6, both phases). Import OP25 `keys.json`
      and dsd-fme CSV key files
- [ ] Later: DMR (Basic Privacy, ARC4 / DES / AES) and NXDN (scrambler,
      DES / AES) on the same keyring

## Where things stood (before this branch)

- Encryption was detected, and the audio dropped. Phase 1:
  `VoiceTracker::voice_frame` returned before the vocoder. Phase 2: each
  codeword was skipped. `Transmissions::kept` dropped encrypted
  transmissions.
- Only the ALGID was used. The KID was decoded but ignored, except in the
  `trunk-pro tool` dump. The Phase 1 MI was decoded, then dropped. The
  Phase 2 MI (MAC_PTT bytes 1–9, ESS-B hexbits 4–15) was never parsed.
- Every implementation XORs the keystream onto the voice bits *after* FEC:
  the 88 IMBE bits u0..u7, or the 49 AMBE+2 bits. That is exactly what the
  trackers hand the vocoder (`imbe_params_to_bits`, `decode_vcw`), so
  decryption is a step there and nothing upstream changes.

## Algorithms

| ALGID | Cipher | Here |
|---|---|---|
| 0x80 | clear | |
| 0x81 | DES-OFB | yes |
| 0x83 | 3-key 3DES-OFB | yes (dsd-fme does Phase 1; nobody does Phase 2) |
| 0x84 | AES-256-OFB | yes |
| 0x89 | AES-128-OFB | yes |
| 0xAA | Motorola ADP (40-bit RC4) | yes |
| 0x82 2-key 3DES, 0x85 AES-128-ECB?, 0x88 AES-CBC, 0x9F DES-XL, 0xA0–A3 DVI-XL/DVP, 0xAB CFX-256, 0xAF AES-256-GCM | | reported as unsupported |

DES and AES come from RustCrypto (`des`, `aes`: pure Rust, no_std,
WebAssembly-friendly). OFB is the block cipher run on its own output, and
RC4 is hand-written.

### Message indicator

- 72 bits are sent; 64 (MI[0..8]) are used.
- LFSR C(x) = x^64 + x^62 + x^46 + x^38 + x^27 + x^15 + 1. Each step the
  feedback (bits 63, 61, 45, 37, 26, 14) is shifted in at the bottom.
  - The next superframe's MI: 64 steps, then MI[8] = 0. Used when an
    LDU2's (or Phase 2 ESS's) encryption sync fails its FEC.
  - AES's 128-bit IV: the 64 bits shifted out over 64 steps, MSB first,
    followed by the final 64-bit state.
  - The LFSR can be inverted (it has an x^0 term), so a late joiner can
    step the first good MI back one superframe (item 7).

### Keystream layout

One keystream per superframe (Phase 1: an LDU1 + LDU2 pair; Phase 2: the
4 × 4V + 2V of one slot), built from the key and the current MI.

- Throw away the first block. ADP discards 256 bytes; DES and 3DES 8; AES 16.
- **Phase 1**: frame k (0–8) of LDU n (0, 1) takes 11 bytes from
  `discard + 101·n + 11 + 11·k + (k == 8 ? 2 : 0)`. That is ADP's familiar
  267 = 256 + 11. The first 11 bytes skip the LC/ES and reserved bytes, and
  the +2 before frame 8 skips the low-speed data.
- **Phase 2**: frame f (0–17: 4 per 4V burst, then 2 for the 2V) takes 7
  bytes from `discard + 7·f`. The last byte keeps only its top bit (49 bits).
- ADP: the RC4 key is the 5-byte key followed by MI[0..8] (13 bytes).
- Which sync applies:
  - Phase 1: the HDU's for the first superframe, then each LDU2's for the
    *next* superframe. It is promoted after that LDU2's voice is decoded.
  - Phase 2: the MAC_PTT's for the first superframe, then the ESS for the
    next. It is promoted after the 2V burst.

## Integration design

- `p25::crypt` is pure, with no engine dependencies. `Keystream::new(alg,
  key, mi, phase)`, `apply_p1(ldu, frame, &mut [u8; 88])` and
  `apply_p2(frame, &mut [u8; 49])`.
- `Keyring`: (algid, kid) → key, plus optional talkgroup → key overrides.
  `resolve(algid, kid, tg)` → decrypt / no key / unsupported. One
  `Arc<Keyring>` per system.
- **Config**: keys go in their own file, never in the main config (the web
  UI reads and edits that). A system gets `"keys": "keys/dcfd.json"`:

  ```json
  { "keys": [
      { "algId": "0xAA", "keyId": "0x1B50", "key": "1234567890" },
      { "algId": "0x84", "keyId": "0x0002", "key": "<64 hex>", "talkgroups": [1201] }
  ]}
  ```

  Keys are never logged, never sent to plugins, and never returned by the
  web API. The UI shows only the ALG/KID, a fingerprint and use counts.
  Warn when the file is readable by others.
- **Call flow**: a grant says only "encrypted"; the KID arrives on the
  voice channel. When the system has keys, an encrypted call is recorded
  tentatively. The tracker's `Cipher` event then decides: no key → the
  transmission is dropped as before; key → it is kept and the call marked
  `decrypted`. `encrypted` keeps meaning "encrypted on the air".
- **Capture** (`.sdr` / frames): store the encrypted bits plus the real
  ALGID/KID/MI, never the key. That keeps the archive faithful, and a
  capture can be decrypted again offline after a fix, by someone holding the
  key. (To decide: the alternative is decrypted bits, which are simpler to
  play back.)

## Testing without a keyed system

No public P25 recording with a matching key exists; the old OP25 samples
are gone. So:

1. The crypto primitives are checked against the published vectors
   (FIPS 46 / SP 800-67 DES, FIPS-197 / SP 800-38A AES-OFB, RFC 6229 RC4).
2. Round trip: take clear voice from our DCFD / WMATA captures, encrypt the
   IMBE / AMBE bits with a known key and MI, re-encode the FEC, and write
   the sync into the HDU / LDU2 / MAC_PTT / ESS (`p25::synth`).
3. Cross-check: run the same synthetic IQ through OP25 master (`-k
   keys.json`) and dsd-fme (`-1` / `-H`). Matching audio proves that our
   offsets agree with the tools used in the field. The round trip alone
   only proves that we agree with ourselves.
4. In the field: ship it marked experimental, per algorithm. Ask testers
   for the dashboard's ciphers line and a ciphertext capture.

## Decisions (2026-10-12, from the user)

- **A whole LDU2 (or Phase 2 2V burst) lost to a fade:** don't guess.
  Stop trusting the sync and wait for the next good one. The tracker today
  steps the MI with the LFSR only when a frame *arrives* with undecodable
  sync; it does not yet notice a frame that never arrived. That detection
  (LDU1 followed by LDU1, or a gap in time) is still to do.
- **Keys shorter than the cipher needs:** refuse them. Loading fails
  with a clear message that names the key ID, the algorithm, and the
  length expected versus given. Keys are not padded the way OP25 pads
  them. `Keystream::new` already returns None on a wrong length; the
  config loader must turn that into the error.
- **A Reed–Solomon encoder** is to be written, so that `p25::synth` can
  build test signals. Only decoders exist (`p25::fec::rs_decode`,
  `rs_decode_erasures`). Needed: RS(24,12) LDU1 LC, RS(24,16) LDU2 ES,
  RS(36,20) HDU, RS(44,16) Phase 2 ESS, RS(63,35) MAC.

## Decisions made while building items 5 and 6 (open to change)

- **A lost sync costs only the voice lost with it.** Refines the
  decision above: still no guessing, and still waiting for the next good
  sync. But that sync (an LDU2's ES, a 2V's ESS) carries the *next*
  superframe's MI, and the LFSR run back once gives the MI of the
  superframe it arrived in. So voice that has no sync (joined late, or the
  previous LDU2 / 2V lost) is held, up to one LDU1 or four 4V bursts, and
  decrypted when the sync comes. If it never comes (the transmission ends,
  or another sync is lost first), it is muted as before. The voice held
  is delayed by up to one superframe (fine for recordings; live audio
  lags by as much).
- **Phase 2 joined late with no MAC_PTT:** whether the voice is encrypted
  is unknown until the first ESS, so it is held until then (with or
  without keys). Before, that superframe's ciphertext was played as
  clear. Held voice of a clear call is released at the ESS, in order.
- **Phase 2 burst count:** a 4V counted as the fifth burst (the op25 count
  only corrects forward) means the 2V was lost and the 4V starts the next
  superframe; it is counted as burst 0. A frame whose place in the
  superframe isn't known is never decrypted (it would index past the
  keystream).
- The Phase 2 synth builds bursts as framed slot packets, not IQ (no
  H-DQPSK modulator) and no MAC_PTT encoder: the tests start without a
  PTT, which exercises the late-join path. The MAC_PTT's MI parse was
  already checked on real air (item 2).

## Decisions made while building item 3 (open to change)

- **Key lookup:** a key that lists `talkgroups` is used for those
  talkgroups under any key ID, or only under its own key ID if it gives
  one. It is tried before the keys matched by key ID alone. A key needs a
  key ID, talkgroups, or both. The same ALGID and key ID given twice
  (without talkgroups) is an error.
- **Keys file format:** `algId` and `keyId` are hex in a string (`"0xAA"`,
  `"AA"`) or a plain number. The key is hex, and spaces are allowed.
- **Keys are read when recording starts.** A file that can't be read, or a
  bad key in it, stops the start with a message naming the system, the
  file, the key's position, its key ID and what is wrong. Editing the file
  needs a restart of recording (`Engine::set_keys` exists for a later live
  reload). On Unix, a file readable by group or others gets a warning in
  the log.
- **Lost sync mid-transmission:** once a transmission has been decrypted,
  voice that can't be (its sync lost) is fed to the vocoder as erasures,
  which repeat and then mute. That keeps the call's timing. Without a key
  the frames are dropped, as before.
- **Frame capture (`.sdr`)** gets the *decrypted* IMBE bits for now (the
  muted frames get zero bits, flagged erased). The `.sdr` has no field for
  the ALGID/KID/MI, so ciphertext could not be decrypted again later.
  Revisit when the capture format can carry the sync.
- **No key fingerprint yet.** A short hash of a 40-bit ADP key would make it
  easy to brute-force. If the UI needs one, use something slow, or show
  only the ALG/KID and the use counts.

## Decisions made while building item 4 (open to change)

- **Which calls are recorded tentatively:** with keys, an encrypted call is
  recorded (even with `recordEncrypted` off) when it is P25
  (`Call::decryptable`; Phase 2 since item 6). DMR and NXDN are still only
  followed. A call with no matching key keeps its
  recorder to the end and is then not saved (empty audio). Releasing the
  recorder as soon as the `Cipher` event shows there is no key would save
  recorders; not done.
- **`encrypted` keeps meaning "encrypted on the air".** `decrypted` is a
  separate flag on the call, in the call JSON (`"decrypted": 0/1`), the
  dashboard's live calls and the plugin `CallInfo` / `CallRecord`.
  Listen's `playable` allows a call that is encrypted *and* decrypted.
- **Upload plugins** (Broadcastify, Rdio Scanner, the template's call
  log) skip any `encrypted` call, so decrypted calls are not uploaded.
  That is left as is on purpose: rebroadcasting decrypted traffic is a
  policy question for the user. If they want it, the plugins can check
  `decrypted`. That needs the SDK published with the new fields.
- **SDK:** `trunk-recorder-plugin` gained fields (`CallInfo.algid`,
  `key_id`, `decrypted`; `CallRecord.decrypted`), but its version was not
  bumped and nothing was published. Bump to 0.2.2 and publish when
  releasing.

## Handoff (2026-10-12, after items 5 and 6)

**Item 6** (and the late-entry half of 7) added:

- `p25/crypt.rs`: `prev_mi` (the LFSR backwards; tests, including the
  real-air pairs run back).
- `trunk/tdma.rs`: `TdmaTracker::new(seed, keys)`; per slot `ks`,
  `decrypting`, `last_vb` (`lost_2v`), `unsure` and `held`;
  `Slot::voice` (decrypt with frame index 4·burst + j, mute, vocode).
  Tests drive the tracker with `synth::Tx2` packets: clear, every cipher
  (from a late join), no / wrong key, a broken ESS, a lost 2V.
- `trunk/tracker.rs`: `held`, `release_held`, `vocode` (split from
  `voice_frame`); an LDU2's ES run back when the superframe had no sync.
- `p25/synth.rs`: `Tx2` (Phase 2 bursts as slot packets: DUID, ISCH,
  AMBE, ESS with RS(44,16), scrambled), `ambe(n)`.
- `trunk/calls.rs`: `Call::decryptable` takes Phase 2.
- Docs: systems.md, command-line.md, architecture.md/.html, CHANGELOG.
- `research/p25-crosscheck/`: the cross-check's scripts, patches to the
  two tools, keys, and how to build and run them.

**Item 5** added:

- `p25/fec.rs`: `rs_encode` (any of the shortened RS codes over GF(64),
  the decoder's layout) and `hamming1063_encode`. Tests: every P25 RS
  code round-trips and corrects up to its limit.
- `p25/voice.rs`: `imbe_header_encode` (u0..u7 → the 144-bit codeword,
  PN masks included), `imbe_bits_to_params` (moved from the tracker's
  test), `bytes_to_hexbits`.
- `p25/synth.rs` (new): `Tx` (NAC, talkgroup, source, optional cipher +
  key, IMBE voice) → HDU, LDU1/LDU2 pairs, TDU as dibits, each LDU2 with
  the next superframe's MI by the LFSR; `modulate` (C4FM, raised-cosine
  α 0.2 frequency pulses); `imbe(n)` test frames. Tests run IQ through
  the real Phase 1 voice decoder (`trunk::voice::build`, receivers,
  framer, tracker): every cipher decrypts back to the bits sent; wrong
  key ≠ clear; no key → nothing; an LDU2 with a broken ES is predicted by
  the LFSR with no loss; a lost LDU2 mutes exactly the superframe it was
  for; in noise (≈ 1 bit corrected per codeword) 102 of 108 frames are
  exact.
- `dsp/filters.rs`: `rc` (moved from `msd.rs`).
- `trunk/conventional.rs`: **bug fix.** A conventional call never got its
  cipher: the HDU (which carries it) is often decoded in a run with no
  voice, and such runs were dropped. Now `Open.cipher_ahead` keeps it
  for the voice that follows. Test `p25_decrypted_calls_name_their_cipher`
  (an engine with a synthetic encrypted conventional channel). Trunked
  calls weren't affected (their call exists from the grant).
- `trunk-pro tool p25synth` (`p25synth.rs`): writes an encrypted (first
  key of `--keys`) or clear capture, cu8 or cf32, from a saved call's
  frames (`--voice`) or made-up ones, and `--wav` 48 kHz discriminator
  audio for DSD-style decoders. Documented in `docs/command-line.md`.

`replay` of a 240 kS/s capture recorded nothing: not a decryption
problem. Fixed after item 6 (`floor_slices`): the conventional noise
floor's 64 slices were 3.75 kHz wide at 240 kS/s, filled by the signal.

## Handoff (2026-10-12, after item 4)

**Item 4** added:

- `trunk/voice.rs`: `TrackerOut::Decrypted` (once a transmission, before
  its first decrypted audio; sent by `VoiceTracker::voice_frame`).
- `trunk/record.rs`: `Transmissions` keeps `TxFlags { encrypted,
  decrypted }` per transmission; `mark_decrypted`, `current_dropped`
  (renamed from `current_encrypted`). `kept` drops only encrypted ones that
  weren't decrypted. The call JSON gets `decrypted`.
- `trunk/calls.rs`: `Call.decrypted`, `Call::decryptable`,
  `CallManager.can_decrypt`, and `admit`'s new rule. `engine.rs` sets
  `can_decrypt` from the keys (also in `set_keys`) and handles `Decrypted`.
  `conventional.rs` does the same for its calls (live audio too).
- Plugin protocol, `session.rs` (plugin `CallInfo`, the live-call JSON),
  `web/src/protocol.ts`, `listen.ts` (`playable`), `dash/Calls.tsx`
  ("recording (decrypted)").
- `trunk-pro replay --keys <file>` (every system; `main.rs`) and the
  ` ENC DEC` tag on its call-end lines.
- User docs: `configuration/systems.md` (Encryption keys section),
  `conventional.md`, `recording.md`, `guides/recordings.md`,
  `command-line.md`, the developer architecture docs, and the CHANGELOG.

**Item 3:**

**State of the branch.** `p25-crypt`, four commits on top of main's v0.1.9
(items 1–2, these notes, item 3, item 4), worktree `../trunk-recorder-lite-crypt`.
The working tree is clean, apart from an untracked `web/node_modules`
symlink to the main checkout's (it lets `npm run schema` run; never commit
it).

**Item 3** added:

- `p25/crypt.rs`: `Keyring` (`add` with the length check and its errors,
  `lookup`, `keystream`), `Lookup`, `Key` (its Debug leaves out the key).
- `trunk/tracker.rs`: `VoiceTracker::new(.., keys)`; `set_es` builds the
  superframe's keystream, `voice_frame` decrypts and `lost_ldu2` drops the
  sync when an LDU2 went missing (an LDU1 after an LDU1, or a time gap
  that holds one). Tests: `notices_a_lost_ldu2`,
  `decrypts_phase1_voice_with_the_key`.
- `trunk/voice.rs`: `VoiceSpec.keys` (VoiceSpec is no longer `Copy`).
- `trunk/engine.rs`: `SystemConfig.keys`, `ConvSystem.keys`,
  `SysHost.keys`, `Engine::set_keys`. `trunk/conventional.rs`:
  `CallRules.keys`.
- `trunk-app/src/keys.rs` (new): the keys-file parser, its path rule and
  its tests. `config.rs`: `keys` on `System` and `Conventional`, and
  `Config::load_keys`. `session.rs`: `Session::set_keys`.
- `trunk-pro/src/runtime.rs`: loads the keys at start
  (`warn_readable_keys`), and logs how many keys each system has.
- Not done: the web UI has no field for `keys` (hand-edit the config; the
  setting survives the UI's saves). `trunk-pro tool` takes no `--keys`
  (replay does, since item 4).

**Items 1 and 2:** see the log below. Files:

- `crates/trunk-core/src/p25/crypt.rs`: new; the ciphers, LFSR, keystreams, tests
- `p25/phase2.rs`: `MacPtt.mi`, `Ess.mi`
- `p25/voice.rs`: `ALGID_CLEAR` now re-exported from crypt; `hexbits_to_bytes` is pub(crate)
- `trunk/tracker.rs`, `trunk/tdma.rs`: `es` / `es_next` / `cipher` state, `encryption_sync()`, `TrackerOut::Cipher`
- `trunk/voice.rs`: `CipherId`, `TrackerOut::Cipher`
- `trunk/calls.rs`, `engine.rs`, `conventional.rs`, `record.rs`: `Call.cipher`; call JSON `algid` / `key_id`
- `trunk-app/src/stats/radio.rs`: `Tg.ciphers`
- `web/src/protocol.ts`, `web/src/dash/Radio.tsx`, `docs/api/protocol.schema.json`: the "Encryption heard" card
- `trunk-pro/src/tool.rs`: MI in the voice dump
- docs: `docs/guides/recordings.md`, `CHANGELOG.md`

**How it was checked:**

- `cargo test --workspace`: all pass.
- Clippy: the same warnings as main.
- `cargo check -p trunk-web --target wasm32-unknown-unknown`: builds.
- The replay check, against a baseline built from main (not against
  `captures/reference`, which is older than main):
  `BIN=<branch release binary> captures/regress.sh <out>`, then
  `captures/compare.sh <main-out> <out>`. Only the call JSONs differ, by
  the new fields. The scripts and captures live in the main checkout.
- The dashboard card has not been looked at in a browser.

**Remaining** (after items 5 and 6), in order:

1. Item 7's other half: import OP25 `keys.json` (`{"0x370e": {"algid":
   "0x84", "key": ["0x00", …]}}`) and dsd-fme key files.
2. A Phase 2 third-party check would need an H-DQPSK modulator and a
   MAC_PTT encoder in the synth (OP25 decodes Phase 2 AES-256, ADP, DES).
3. DMR / NXDN.
4. Smaller: release the recorder of a tentatively recorded call once its
   `Cipher` shows there's no key; a `keys` field in the web UI; publish
   the SDK.

## Log

- 2026-10-12: plan written; branch started.
- 2026-10-12: items 1 and 2 done.
  - The crypto primitives pass RFC 6229 (RC4), FIPS 81 (DES), SP 800-38A
    (AES-128/256 OFB) and the offset-layout tests.
  - Checked on real air: the reference capture `captures/p2-*.cu8` holds
    DCFD's encrypted traffic, which is **AES-256 (0x84)** under key IDs
    0x370E, 0x4319, 0xD42A and 0x0009, on both Phase 1 and Phase 2. All
    233 superframe-to-superframe MI transitions in it (179 Phase 1 LDU2s,
    54 Phase 2 ESS) equal `next_mi()` of the MI before. That confirms the
    LFSR, the byte order, both MI parsers and when each sync takes effect.
    Four of those pairs are a unit test (`lfsr_predicts_the_air`).
  - Bit order: our IMBE `[u8; 88]` and AMBE `[u8; 49]` match OP25's
    `imbe_pack` / `pack_cw` MSB-first packing, so the XOR runs on them
    directly.
  - The regression captures replay the same as main, apart from the new
    `algid` / `key_id` fields in each call's JSON.
- 2026-10-12: item 3 done. The Phase 1 tracker decrypts with the system's
  keyring (ADP, DES, 3DES, AES-128, AES-256), and keys come from a
  per-system file named by `keys`. A bad key stops the start with a clear
  error. A lost LDU2 drops the sync until the next good one.
  - Checked: `cargo test --workspace` passes, clippy has the same warnings
    as main, and the wasm build works. The replay check against main gives
    identical WAVs and `.sdr` files; only the call JSONs' `algid`/`key_id`
    differ (from item 2).
  - The decryption itself is checked only by unit tests (keystream round
    trip through `voice_frame`). The whole tracker is untested until the
    synth (item 5).
- 2026-10-12: item 4 done. A system with keys records its encrypted Phase 1
  calls, and decrypted transmissions are kept, marked `decrypted`, playable
  in Listen and reported to plugins. User docs and CHANGELOG written.
  - Checked: tests, clippy (same as main), the wasm build and the schema
    check pass. The replay against main gives identical WAVs and `.sdr`
    files; the call JSONs differ only by `algid`/`key_id`/`decrypted`.
  - End to end on real air: `replay --keys` over the DCFD capture
    (`captures/p2-*.cu8`), with an all-zero AES-256 key under each of its
    four key IDs. Every in-range Phase 1 encrypted call (TG 716, 717, 720,
    2147; KIDs 0x370E, 0xD42A) was recorded, "decrypted" and saved with
    `decrypted: 1`. The Phase 2 calls stayed encrypted-only. With the
    wrong key the audio is noise; this proves the flow, not the cipher.
- 2026-10-12: item 5, first part: an RS encoder, `p25::synth` and `tool
  p25synth`. Round trips through the whole Phase 1 path pass for ADP,
  DES, 3DES, AES-128 and AES-256.
  - `tool p25synth --keys` with a real DCFD call's 360 IMBE frames
    (`captures/reference/…/729-63_859037500.frames.jsonl`), then `replay
    --p25 --keys --capture-frames`: all 360 frames come back bit-exact
    under each cipher, and the call JSON has the ALGID and key ID. With a
    wrong key, 0 of 360 match.
  - Found and fixed: conventional calls had `algid`/`key_id` −1 (see the
    handoff).
  - Checked: `cargo test --workspace` passes, clippy only has warnings of
    kinds main already has, the wasm build works.
- 2026-10-12: item 5 done: **cross-checked against OP25 and dsd-fme.**
  `tool p25synth` signals (the same real call's 360 IMBE frames, clear and
  under each cipher) decoded by each tool; decrypted frames compared with
  the same tool's decode of the clear signal. Full method, commands and
  patches: `research/p25-crosscheck/`.
  - OP25 (boatbod master 71abcd0, GNU Radio 3.10.12): ADP, DES-OFB and
    AES-256 decrypt to 360 of 360 frames, 0 of 31,680 bits wrong. It has
    no 3DES or AES-128. It decrypts with the MI from each LDU2; all 20
    logged MI steps follow our LFSR.
  - dsd-fme (audio_work 4fe32db, mbelib 1.3.2): ADP, DES, 3DES, AES-128,
    AES-256 all 360 of 360, 0 bits wrong (from OP25's symbols; from our
    discriminator WAV a few demodulator bit errors, as in its clear
    decode). Two dsd-fme bugs, not disagreements: its AES keystream is 14
    blocks where 15 are needed, so bytes 6–10 of frame 17 of each
    superframe stay ciphertext (with 15: bit-exact); and it mutes 3DES
    audio (its bits are right).
  - Both report the ALGID, key ID and MI as sent. No key or a wrong key:
    about half the bits wrong, garbled or no audio.
  - OP25 also has an audio bug of its own in decrypted calls (u7 not
    shifted back before the vocoder); its logged bits are right.
  - This checks our offsets and ciphers against the tools used in the
    field, for Phase 1. Phase 2 has no third-party check yet (no Phase 2
    IQ synth); its layout (`7·f`) and MIs rest on the item 1 tests and
    the real-air MI checks.
- 2026-10-12: item 6 done: Phase 2 decrypts, and a late join or a lost
  sync costs only the voice lost with the sync, in both phases.
  - Tests: Phase 2 every cipher, no/wrong key, broken ESS, lost 2V;
    Phase 1 late join (HDU missed: the whole transmission decrypts), lost
    LDU2 (only its 9 frames missing).
  - Real air, `replay --keys` with zero AES-256 keys over
    `captures/p2-*.cu8`: every encrypted call, Phase 1 and now Phase 2
    (TG 11033, 11037, 11327), recorded with `decrypted: 1` and its
    ALGID/KID (0x4319, 0x0009 on Phase 2). Wrong key: noise; this proves
    the flow.
  - Regression replays (no keys) against item 4: Phase 1, DMR, SmartNet
    and conventional identical. Phase 2 differs, as intended: clear call
    2191 gains 8 good frames at its start (heard before the grant reached
    the slot, now held and released into the call); encrypted calls no
    longer count the late-joined superframe's ciphertext as audio (one
    stop time 0.9 s earlier); signal/SNR move by ≤ 1.8 dB on encrypted
    calls and ≤ 0.3 dB on clear ones, as the measured span follows the
    voice. Clear calls' frames are otherwise bit-identical.
