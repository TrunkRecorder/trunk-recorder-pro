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
starting at 1; **do not enter logical slot numbers as repeater numbers**.
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
| FID `0x68`, CSBK `0x3D` | Data/control preamble: group/private, blocks to follow, 16-bit target/source, free/priority repeater hints, priority hash and reserved bits; identifies XPT but never grants voice |
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

Data/control preambles are reported as observations. They do not replace the
voice-site free/priority state: in the supplied capture the data carrier's
preambles advertise free repeater 15 while the voice carriers advertise 5.
The meaning of that difference is unverified. Preamble addresses and hashes
never create voice calls, establish home groups or learn frequency mappings.
Protected preambles are not interpreted as clear XPT identification.

The same message stream reports Hytera CSBK `0x0D`, full LC `0x0D`, short LC
`0x0C` with the observed embedded `0x68` manufacturer value, and data terminator
LC `0x30`. The proprietary forms retain their opcode/flags and raw payload;
their field meanings remain unverified. In particular the repeated `0x7FFF`
value is not treated as a site, radio or talkgroup ID. These diagnostics alone
do not identify XPT or alter voice, privacy, availability or site membership.
Data terminators are informational and cannot end a voice call.

For undeduplicated control diagnostics, use the existing tool:

```sh
trunk-pro tool dmr site.cu8 --center 451000000 --rate 2400000 --freq 451012500 --bursts
```

It emits JSON lines for raw CSBK, full LC and short LC, plus decoded XPT fields.
Voice IDs use the XPT interpretation after detection; raw hex stays unchanged.
Additional `hytera` records expose the data terminator and proprietary forms.
`alias` records show complete or partial over-the-air radio names; see
[DMR radio-name decoding](dmr.md#over-the-air-radio-names).
The CLI's `encrypted`/`emergency` fields apply to recognized voice-user LCs;
nonvoice payload bits are not interpreted as voice service options.
Privacy indicators and service-option encryption flags are reported. Encrypted
frames are not vocoded by either the recorder or the tool. No key is requested.

## Evidence and milestones

The initial implementation used synthetic tests and two system-description PDFs.
Subsequent supplied captures now validate part of the receive path:

- A 4.704-second voice tape at 453.5375 MHz decodes TG 149 / RID 5006 on slot 2,
  with 108 good frames and no errors. The pre-XPT build reports TG 4,194,453.
- `20261110-025018/source-3` is 30 seconds of 1.024 MS/s `cu8` IQ centered at
  453.506 MHz (SHA-256 `698af12cfc3685e0b8c9b41187d55130931429149b6bbc2f663c49cbc2e3ef26`).
  Replaying an isolated copy with only source 3 and the configured WCS DMR site
  records TG 149 / RID 5006 on 453.5375 MHz slot 1: 276 and 108 good frames,
  5.52 and 2.16 seconds of audio, no errors. The pre-XPT build reports TG
  5,243,029; both builds produce identical audio. This does not reproduce the
  original no-audio symptom.
- The same IQ contains XPT status/short LC on 453.425 MHz and XPT data preambles
  on 453.6375 MHz. On the latter, 32 valid preambles count down from 18 to 3
  for targets 520 and 620, from source 16, followed by data bursts and
  terminators; there are no voice frames. Proprietary CSBK/LC `0x0D` and short
  LC `0x0C` are retained for further research. Positive XPT identification does
  not prove that independently configured carriers belong to the same site.
- Embedded Hytera alias LC on the voice carrier contains the UTF-16BE prefix
  `Hytera`. Its header declares 13 code units, but only six arrive in the
  header/block-1 sequence; it is exposed as partial and does not replace the
  saved name for radio 5006. Alias format bits are not voice privacy flags.

The supplied ZIPs contain individual radios while the original collection and
configuration reference five. Keep originals intact; use an explicitly isolated
copy/configuration to replay one radio. Captures are not redistributed in git;
the tests include the observed control payloads.

| Evidence | What it supports |
| --- | --- |
| Hytera XPT System Application Notes R5.0, §§1.2 and 4.1 | Home/free repeater model, one free repeater at a time, at most eight voice repeaters, 16-bit radio allocation, group ranges, independently configurable repeater color codes |
| Hytera DMR Systems White Paper, “How XPT Works” and “Multi-Site XPT” | Dynamic sharing of two-slot repeaters and separate sites connected over IP |
| Hytera priority-signaling patent US20180176984A1, table 6 | CSBK `0x3D` preamble layout and 16-bit addressing |
| ETSI TS 102 361-3 §7.1.1.1 (TD_LC) | FLCO `0x30` in a terminator burst is data control, not voice |
| Synthetic payload, FEC and IQ tests | Parser bounds, field extraction, bank/free changes, setup, mapping, late entry, concurrent tracking, termination, stale state, site isolation, privacy and correct recorded IDs across concurrent repeaters |

Milestone 1 is implemented and synthetic-tested: positive signaling identifies
XPT and decodes the supported CSBK/SLCO/FLCO control messages. Milestone 2 has a
working recording path through the existing engine, including synthetic tests
of two concurrent repeaters with different codes and slots, and the limited
real-capture checks above. Broader operation still requires live validation.

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

## Protocol references

The Rust implementation uses this repository's GPL-3.0-or-later license.
No external decoder is linked or vendored. No Hytera PDFs are redistributed.

- [Application notes](https://www.hamdigitaal.nl/download/algemene-informatie/XPT-System_Application-Notes_R5.0.pdf)
- [White paper](https://www.hytera.us/wp-content/uploads/2023/10/Hytera_DMR_Systems_White_Paper_vA.pdf)
- [Hytera priority-signaling patent, table 6](https://patents.google.com/patent/US20180176984A1/en)
- [ETSI TS 102 361-2 V2.5.1 (talker aliases)](https://www.etsi.org/deliver/etsi_ts/102300_102399/10236102/02.05.01_60/ts_10236102v020501p.pdf)
- [ETSI TS 102 361-1 V2.7.1](https://www.etsi.org/deliver/etsi_ts/102300_102399/10236101/02.07.01_60/ts_10236101v020701p.pdf)
