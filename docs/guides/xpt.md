# Hytera XPT (receive only)

Select **DMR (trunked)**. XPT uses the existing 4800-baud DMR receiver,
burst framing, FEC, two-slot transport, AMBE+2 decoder and recording pool.
No transmitter, registration, affiliation or decryption is implemented.

## Configuration

Each configured site owns independent XPT state. List only repeaters belonging
to that site, and provide SDR coverage for all of them. A repeater is a physical
carrier with two slots; its **LCN is not its logical slot number (LSN)**:
`LSN = 2 * (LCN - 1) + slot`, where the displayed slot is 1 or 2 (the API uses 0 or 1).

Example site settings within a system with `"type": "dmr"`:

```json
{
  "controlChannelsHz": [451000000],
  "dmrChannelsHz": [451012500],
  "lcnTableHz": {"1": 451000000, "2": 451012500},
  "dmrColorCodes": {"451000000": 1, "451012500": 2},
  "xptHomeGroups": {"25": 1, "26": 2}
}
```

`controlChannelsHz` names the initial/home frequencies to monitor, not a
permanent XPT control channel. Every frequency in that list, `dmrChannelsHz`,
and `lcnTableHz` is watched simultaneously. Mapping keys are repeater numbers,
starting at 1; **do not enter the DSD-FME logical-slot map as a repeater map**.
Calls can still be found by voice LC on explicitly listed frequencies without
a map; the repeater number remains unknown. XPT does not learn mappings from
status target hints, because private-target hashes are not unique IDs.

`xptHomeGroups` records the home-group programming supplied by the operator.
Calls on overflow repeaters never change this table. The implemented on-air
messages do not establish authoritative home-group ownership.

Hytera permits different color codes on repeaters in one site. Leave the global
`colorCode` unset in that case and use `dmrColorCodes` for explicit per-frequency
filters. A per-frequency code overrides the global code. Without explicit filters,
XPT site membership is the configured frequency list. A neighbor announcement
never merges its activity or changes the current site's identity. Do not put
unrelated systems or different sites in one site configuration. Co-channel sites
with identical color codes cannot be reliably separated by the implemented
signaling; configuration and RF coverage must distinguish them.

The normal recording pool and source bandwidth determine concurrency. Both
slots and multiple configured repeaters can be decoded at the same time. Calls
are opened/refreshed from voice LC on their actual carrier and slot. This avoids
recording a target hash, data indication or unconfirmed alert as a voice call.
The existing pre-roll preserves the beginning of a transmission when available.
A receiver covering only the home frequency cannot follow an uncovered repeater.

## Decoding and tracking

`dmr::xpt` is a pure payload parser plus a separate site-local tracker. It receives
integrity-checked payloads from the existing slot decoder. The common
`dmr::short_lc` transport assembles first/continuation/continuation/last CACH
fragments, deinterleaves BPTC(68,36), corrects Hamming rows, checks column parity
and CRC8, and emits short LC. An incomplete sequence is discarded.

| Message | Implemented fields / behavior |
| --- | --- |
| FID `0x68`, CSBK `0x0A` | Sequence (top two bits, not protect/LB flags), free repeater, six two-bit slot states and eight-bit target hints; banks 0–2 remain separate |
| FID `0x68`, CSBK `0x0B` | Four neighbor site numbers, free repeaters, reserved fields; informational only |
| SLCO `0x08` | Free repeater, priority repeater and target hash; no FID in short LC |
| FID `0x68`, FLCO `0x09` | Setup/alert repeater, encoded slot, group/private, 16-bit target/source, free repeater, handshake and reserved fields |
| Voice FLCO `0x00` / `0x03` | Confirmed group/private call on received carrier and slot, target/source, privacy and emergency options |
| Voice terminator | Ends the matching tracker call; normal recorder hang-time rules apply |
| Privacy indicator | Marks encrypted traffic; the existing decoder suppresses encrypted audio |

**TGID / RID correction:** after positive XPT identification, voice target bits
32–47 and source bits 56–71 are used. Bytes at bits 24–31 and 48–55 are not part
of either ID. Previously the ordinary 24-bit LC interpretation could turn those
bytes into very large TGIDs/RIDs. Both the control and voice paths now use the
same XPT interpretation, including a newly opened receiver which has not yet
heard setup itself: the control channel passes XPT context through `VoiceParams`.
Raw payloads are preserved. FID `0x68` alone does not establish XPT; ordinary
Hytera/standard DMR keeps its 24-bit interpretation. Before any positive XPT
signaling is heard, this distinction remains unknown.

A setup carried in a terminator burst is **not** a voice termination. A setup is
pending until actual voice LC arrives; it expires after four seconds. Free,
priority, neighbor and bank observations and confirmed tracker calls expire
after three seconds without refresh. A zero free-repeater value clears the
previous assignment. A delayed terminator naming a different call cannot end
the replacement tracker call. Slot state values and private-target hashes remain
raw observations; they cannot override a confirmed call's identity or encryption.

## Diagnostics

The normal decoded-message stream includes deduplicated XPT status records with
raw hex and all parsed control fields. The site's DMR API status includes `xpt`
(home groups, free/priority observations, occupancy by logical slot, neighbors,
pending setups and confirmed calls). `availableRepeaters` lists repeaters with
at least one fresh, explicitly idle slot; unknown/private/inactive states do not
imply availability. Each observation carries its receive time.

For undeduplicated control diagnostics, use the existing tool:

```sh
trunk-pro tool dmr site.cu8 --center 451000000 --rate 2400000 --freq 451012500 --bursts
```

It emits JSON lines for raw CSBK, full LC and short LC, plus decoded XPT fields.
Voice IDs use the XPT interpretation after detection; raw hex stays unchanged.
Privacy indicators and service-option encryption flags are reported. Encrypted
frames are not vocoded by either the recorder or the tool. No key is requested.

## Evidence and milestones

No actual XPT IQ/audio captures were supplied with this change. The two supplied
PDFs are system descriptions, not over-the-air test vectors. **No XPT behavior
is claimed as capture-validated yet.**

| Evidence | What it supports |
| --- | --- |
| Hytera XPT System Application Notes R5.0, §§1.2 and 4.1 | Home/free repeater model, one free repeater at a time, at most eight voice repeaters, 16-bit radio allocation, group ranges, independently configurable repeater color codes |
| Hytera DMR Systems White Paper, “How XPT Works” and “Multi-Site XPT” | Dynamic sharing of two-slot repeaters and separate sites connected over IP |
| DSD-FME `audio_work` revision `34753963b8855c6c07b79250cb4de4f2c38a4613`, `src/dmr_csbk.c`, `src/dmr_flco.c` | Proprietary bit positions, sequence banks, 16-bit LC addressing, setup in terminator LC, uncertain status meanings and lack of reliable current-site identification |
| Synthetic payload, FEC and IQ tests | Parser bounds, field extraction, bank/free changes, setup, mapping, late entry, concurrent tracking, termination, stale state, site isolation, privacy and correct recorded IDs across concurrent repeaters |

Milestone 1 is implemented and synthetic-tested: positive signaling identifies
XPT and decodes the supported CSBK/SLCO/FLCO control messages. Milestone 2 has a
working synthetic-tested recording path through the existing engine, including
two concurrent repeaters with different codes and slots. Reliable operation on
real systems still requires capture replay and live validation.

Remaining unverified behavior includes reserved slot states (especially state 1),
private target-hash correlations, nonzero priority fields, setup handshake/release
semantics, enhanced/privacy-specific Hytera FLCOs, and firmware variations. The
setup reserved nibble is not treated as an authoritative call-state or privacy
flag. Status bank 2's last entries are retained as raw observations, not assumed
to indicate a ninth voice repeater. SLCO transport conservatively rejects residual
column-parity errors. The supplied ETSI URL could not be fetched because its host
was blocked by the environment policy; independent review against that edition
remains outstanding.

To validate on captures, replay known clear group and private calls, verify the
expected TGID/RID against radio programming, exercise both slots and overflow
repeaters, then compare raw control traces and recordings through setup, hang
time, lost signaling, free-repeater changes and encrypted calls. Keep a separate
site configuration for each site and include same-code neighbor tests.

## Sources and licensing

The Rust implementation is original; no DSD-FME decoder is linked or vendored.
Protocol field research credits DSD-FME / LWVMOBILE (2022–2023), DSD Author,
and the upstream projects acknowledged by DSD-FME. Its copyright/permission
notice is retained in [DSD-FME.txt](../licenses/DSD-FME.txt). This repository's
GPL-3.0-or-later license remains in force. No Hytera PDFs are redistributed.

- [Application notes](https://www.hamdigitaal.nl/download/algemene-informatie/XPT-System_Application-Notes_R5.0.pdf)
- [White paper](https://www.hytera.us/wp-content/uploads/2023/10/Hytera_DMR_Systems_White_Paper_vA.pdf)
- [Pinned DSD-FME](https://github.com/lwvmobile/dsd-fme/tree/34753963b8855c6c07b79250cb4de4f2c38a4613)
- [ETSI TS 102 361-1 V2.7.1](https://www.etsi.org/deliver/etsi_ts/102300_102399/10236101/02.07.01_60/ts_10236101v020701p.pdf)
