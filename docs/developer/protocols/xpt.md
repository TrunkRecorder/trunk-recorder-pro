# Hytera XPT: protocol notes

How the recorder decodes Hytera XPT (receive only), what the decoder's tables rest on, and what is
still unverified. Setting up an XPT site is in [Trunked DMR](../../guides/dmr.md#hytera-xpt). The
code is `crates/trunk-core/src/dmr/xpt.rs`.

## Decoding and tracking

`dmr::xpt` parses integrity-checked payloads from the slot decoder;
`dmr::trunking::Site` puts them into the site state every DMR variant uses.
The common `dmr::short_lc` transport assembles first/continuation/continuation/last
CACH fragments, deinterleaves BPTC(68,36), corrects Hamming rows, checks column
parity and CRC8, and emits short LC. An incomplete sequence is discarded.

| Message | What the recorder does with it |
| --- | --- |
| FID `0x68`, CSBK `0x0A` | Status bank (sequence 0–2, six slots each): its free repeater is the site's rest channel; busy slots and their 8-bit target hints go in the site status line |
| FID `0x68`, CSBK `0x0B` | Neighbour sites and their free repeaters: a status line |
| SLCO `0x08` | Free repeater (the rest channel); priority repeater and target hash are not used |
| FID `0x68`, FLCO `0x09` | Call setup: repeater, slot, group/private, 16-bit target/source, free repeater. A status line, and like a grant it lets an unlisted repeater number be learned; it is not a call |
| FID `0x68`, CSBK `0x3D` | Data/control preamble: one status line per transfer (each preamble counts down the blocks to come). Identifies XPT; never a voice grant, never the free repeater |
| Voice FLCO `0x00` / `0x03` | The call, on the carrier and slot it is heard on, with 16-bit IDs |

A setup is often carried in a terminator burst; it is **not** the end of a call.

**TGID / RID width:** after positive XPT identification, voice target bits
32–47 and source bits 56–71 are used; the bytes above them are not part of
either ID (the ordinary 24-bit reading turns them into very large TGIDs/RIDs).
The DMR channel decoder applies it, once its carrier is known to be XPT's: from
XPT signalling heard on it, from its site, or (a voice channel opened later)
from the control channel's `VoiceParams`, which carry the site's DMR variant.
FID `0x68` alone does not establish XPT; ordinary Hytera/standard DMR keeps its
24-bit IDs.

## Raw control with `tool dmr`

```sh
trunk-pro tool dmr site.cu8 --center 451000000 --rate 2400000 --freq 451012500 --bursts
```

It emits JSON lines for raw CSBK, full LC and short LC (with their hex), plus
decoded `xpt` fields. After detection, voice LC is shown as the decoders get
it: 16-bit IDs, the bytes above them cleared. Observed Hytera data-channel
profiles also produce `hyteraData` records: beacon CSBK `0x0D`, setup LC `0x0D`,
short status LC `0x0C`, and end LC `0x30`. The records expose verified radio/slot
fields, the raw activity word and an experimental `busyHint`, while preserving
the full raw bytes. They do not create voice grants or channel mappings. See
[DMR data signalling](dmr-gps.md#data-channel-signalling-follow-up).
`alias` records
show the over-the-air radio name as it comes together, whole or partial; see
[DMR radio-name decoding](../../guides/dmr.md#over-the-air-radio-names). The tool's voice,
its muting of encrypted frames and its aliases come from the recorder's own DMR
voice decoder.

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
- Corpus capture `4aa34b203c13d715` (the same system, radio 3) and voice tapes
  `a7a6bb2cf35d069a` and `b69007e66f75c602` replay to TG 149 / RID 5006, with
  the same audio as before XPT support; the setup learns repeater 4 as
  453.5375 MHz. The free repeater (5) and repeaters 1–3 stayed unmatched to a
  frequency in its 30 seconds, and the alias again stops at `Hytera` (blocks 0
  and 1 of a 13-unit name).

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
| Synthetic payload, FEC and IQ tests | Parser bounds, field extraction, free repeater, setup learning, per-repeater colour codes, data preambles, and correct recorded IDs across concurrent repeaters |

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
