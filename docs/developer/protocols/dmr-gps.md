# DMR and Hytera GPS positions

The recorder decodes GPS Info full link control embedded in voice calls (FLCO
`0x08`), for standard DMR (FID `0x00`) and Hytera (FID `0x68`), including XPT.
No configuration is required. Positions use the same `TrackerOut::Position`,
call JSON `srcList` latitude/longitude fields, and `Event::UnitPosition` path as
P25. Decoder source names are `DMR` and `HyteraDMR`.

The existing embedded-LC FEC and checksum handling, including keyed-system
handling, runs first. A position belongs to the current slot's talker, named
by voice LC less than four seconds earlier. A voice terminator (including
Hytera FLCO `0x30`) clears it. Protected words, unknown/zero talkers, and `(0, 0)`
are ignored. A fix on only the equator or prime meridian is accepted. Accuracy
`7` means unknown accuracy; it does not alone invalidate a nonzero position.
Positions are read even when the call's audio is encrypted.

The recorder also assembles unconfirmed defined short-data packets (SAP 10,
DPF 13) carried in rate-1/2 blocks. Supported Hytera applications are location
(LP) reports, as text and (experimentally) compressed, and RRS
registration/offline/status messages. Other packet applications stay visible as
raw data. Motorola LRRP, UDT/NMEA, confirmed data,
fragmented messages, and rate-3/4 or rate-1 data are not implemented here.

## Wire format and sources

Ranges below exclude their end; bit 0 is the first byte's MSB. Coordinates
use two's complement, not sign/magnitude; north/east are positive.

| Bits | Meaning |
| --- | --- |
| 0..8 | Protect/reserved flags and six-bit FLCO `0x08` |
| 8..16 | Standard or Hytera FID |
| 16..20 | Reserved |
| 20..23 | Position error / accuracy |
| 23..48 | Signed longitude × 360 / 2²⁵ degrees |
| 48..72 | Signed latitude × 180 / 2²⁴ degrees |

The layout and constants agree in ETSI, SDRTrunk (`GPSInformation.java`),
ok-dmrlib (`full_link_control.py`), dmr-kaitai (`full_link_control.ksy`) and
dsd-fme (`dsd_gps.c`). dsd-fme takes a negative value's magnitude as
`0x800001 - v` / `0x1000001 - v`, one step (about 1 m) too far; two's complement
here is the spec's. FLCO 8 is GPS only for FID `0x00` and `0x68`: Tait (`0x58`)
uses it for its talker alias, and Motorola (`0x10`) for something else.

Sources researched for the original implementation:

- [ETSI TS 102 361-2 V2.4.1](https://www.etsi.org/deliver/etsi_ts/102300_102399/10236102/02.04.01_60/ts_10236102v020401p.pdf),
  §7.1.1.3 / Table 7.3 and §§7.2.15–17: normative standard DMR layout and coordinate units.
- SDRTrunk [LCOpcode](https://github.com/DSheirer/sdrtrunk/blob/master/src/main/java/io/github/dsheirer/module/decode/dmr/message/data/lc/LCOpcode.java),
  [LCMessageFactory](https://github.com/DSheirer/sdrtrunk/blob/master/src/main/java/io/github/dsheirer/module/decode/dmr/message/data/lc/LCMessageFactory.java),
  and [GPSInformation](https://github.com/DSheirer/sdrtrunk/blob/master/src/main/java/io/github/dsheirer/module/decode/dmr/message/data/lc/full/GPSInformation.java):
  evidence that Hytera uses the same layout with FID `0x68`; also identifies
  Hytera's terminator opcode.
- SDRTrunk [issue #1907](https://github.com/DSheirer/sdrtrunk/issues/1907):
  Hytera XPT hemisphere errors, covered here by signed-coordinate tests.

The Hytera interpretation is supported by implementation evidence; ETSI does
not specify that vendor extension.

## Compatibility and validation

Updated against upstream v0.5.0, commit `a5c4376`.
Upstream already supplies `Position`, `CallSource.position`,
`CallManager::note_position` and JSON serialization to `srcList[].lat`/`lon`
for P25. This change adds DMR/Hytera decoders and the standalone packet-data
routing into that infrastructure; it does not replace the JSON writer.

The release normalizes XPT voice IDs in `Channel`, assembles talker aliases in
`DmrVoice`, and vocodes both slots without the old `vocode` switch. GPS keeps
those behaviors and shares the existing voice-LC timestamp for attribution.

Tests cover standard/Hytera coordinates in all hemispheres, no fix and protected
words, slot isolation, transmission end, stale talkers and refresh, and synthetic
4FSK through embedded-LC FEC. Engine tests verify position events and saved call
JSON for both conventional and trunked DMR; XPT tests verify normalized radio
IDs without truncating coordinate bits. Release-specific tests interleave GPS
and alias blocks to verify both outputs survive.


## Where a position goes

Every position, embedded or sent as data, goes through `CallManager::position`:
it's kept on each call its radio is on (`srcList[].lat`/`lon`), and an
`Event::UnitPosition` reports it with that call's talkgroup, or `0` when the
radio is on no call (a report between calls). A slot that carries only a
position starts no call; on a conventional frequency with codes, the rows'
codes admit it as they do voice.

## Hytera application messages (HDAP)

Hytera radios put their applications in short data as an HDAP message after
two octets of their own (`01 00`, `02 00`; dmrshark calls them pre-padding,
and what they say isn't known):

| Octets | Meaning |
| --- | --- |
| 0 | Header: top bit asks for an acknowledgement; low 7 bits the protocol (`0x08` LP, `0x11` RRS; `0x01` the compressed reports below) |
| 1..3 | Opcode, big endian |
| 3..5 | Body length, big endian |
| 5..5+n | Body |
| 5+n | Checksum: octets 1..5+n summed, complemented, plus `0x33` |
| 6+n | `03` |

`dmr::hytera::hdap` checks the length, checksum and end before anything is
read (dmr-kaitai `hytera_dmr_application_protocol.ksy`; ok-dmrlib `hdap.py`).

### Location reports as text (LP, protocol `0x08`)

An answer to a location request (`A002`), and emergency (`B003`), triggered
(`C003`) and condition (`D003`) reports end with the GPS data, 40 ASCII octets
(dmr-kaitai `location_protocol.ksy` and `gpsdata.ksy`; dmrshark `dmr-data.c`
reads the same fields at fixed offsets, and dsd-fme's `dmr_locn` by scanning
for the letters):

| Octets | Field |
| --- | --- |
| 0 | `A` a fix, `V` none (no position) |
| 1..7 | Time, HHMMSS UTC |
| 7..13 | Date, DDMMYY |
| 13 | `N` or `S` |
| 14..23 | Latitude, DDMM.MMMM |
| 23 | `E` or `W` |
| 24..34 | Longitude, DDDMM.MMMM |
| 34..37 | Speed, knots |
| 37..40 | Heading, degrees from north |

The position is the packet header's source radio's, any hemisphere. The
decoder name is `HyteraDMR_LP`. Only generated vectors test it so far: a capture
of a radio with GPS compression off would confirm it on the air.

## Experimental compressed Hytera GPS and packet services

This is a capture-derived extension, not a published Hytera specification
(dmrshark recognises the same marker and doesn't decode it). Its first three
octets read as an HDAP header, protocol `0x01` with LP's condition-report
opcode `D003`; no length or checksum has been found after them. The marker
is `01 D0 03 00`, following the two-byte short-data prefix `01 00`. The observed application payload is 27 bytes:

| Payload bytes (end excluded) | Interpretation |
| --- | --- |
| 0..2 | Short-data prefix `01 00` |
| 2..6 | Compressed LP marker `01 D0 03 00` |
| 6..9 | Preserved raw; not assigned a field meaning |
| 9..15 | Binary UTC hour, minute, second, day, month, two-digit year |
| 15..19 | Little-endian latitude word |
| 19..23 | Little-endian longitude word |
| 23..27 | Preserved raw; motion-field units remain unverified |

Only the observed coordinate flag combination (high nibble `8` on both words)
is enabled, interpreted as north/west for the capture-validated profile. Other flag combinations are retained in raw messages and do **not**
emit positions. The low 28 bits contain `degrees * 1,000,000 + minutes * 10,000`.
Decimal degrees are `degrees + minutes / 60`, not the stored integer divided
by a million. Minute ranges, coordinate bounds, dates, payload size, and a
nonzero header source are checked. This limited profile is experimental:
additional independently located captures, particularly other hemispheres and
no-fix reports, are needed to establish the complete flag/fix-status semantics.
The source name is `HyteraDMRCompressed`.

A packet's source radio comes from its 24-bit header, independently for each
slot. Headers require their actual masked CRC16 even when the voice decoder
has detected keyed CRCs. The bounded assembler accepts at most 63 blocks and
expires after four seconds. A new header, invalid/unsupported data block, or
voice transmission interrupts assembly. The full CRC32 must pass before any
application is decoded; CRC32 covers padding, exchanges adjacent octets,
uses polynomial `0x04C11DB7`, initial zero and no final complement, with a
little-endian check on the air. Header-declared octet-aligned bit padding and
the CRC are then removed.

Packet positions go where every position does ([above](#where-a-position-goes)):
they do not create or prolong a voice call. Configure the data frequency as a
site's watched frequency (or a conventional DMR channel);
the decoder cannot discover an unconfigured dedicated data frequency from
these vendor words.

RRS packets are HDAP protocol `0x11` (after `02 00`). Their opcode,
length, HDAP checksum and ending `03` are checked in addition to the air CRCs.
Registration request `0003`, answer `0080`, offline `0001`, status check `0002`
and status answer `0082` are recognized. Messages retain both the over-the-air
sender and the radio IP from the application; a dispatcher's reply is not
attributed to the radio it answers. Result and status codes remain numeric;
registration replies include the renewal interval in seconds.

The additional observed Hytera CSBK opcodes `08`/`0D`, full LC `0D`/`30`, and
short LC `0C` are not reported by the recorder (their fields aren't
documented); `trunk-pro tool dmr` prints the profiles below. They are not used
to infer voice grants, repeater mappings, radio IDs, or GPS. Short LC has no
manufacturer identifier, so opcode `0C` alone is not evidence of XPT.

`trunk-pro tool dmr` prints checked packet headers/payloads, application
summaries, and positions as JSON lines. User IQ captures and exact locations
are not included in the repository. Anonymous golden vectors and synthetic
4FSK tests cover the transport and complete engine path.

Additional sources consulted:

- [ETSI TS 102 361-1 V2.5.1](https://www.etsi.org/deliver/etsi_ts/102300_102399/10236101/02.05.01_60/ts_10236101v020501p.pdf),
  §8.2.1: data header formats and short-data transport.
- OK-DMR [data_header.py](https://github.com/OK-DMR/ok-dmrlib/blob/master/okdmr/dmrlib/etsi/layer2/pdu/data_header.py):
  appended-block, full-message, SARQ, SAP and padding field positions.
- dmrshark [dmr-handle.c](https://github.com/nonoo/dmrshark/blob/master/libs/base/dmr-handle.c):
  identifies the compressed Hytera marker and short-data prefix; explicitly
  does not decode the compressed coordinates.
- OK-DMR [radio_registration_service.py](https://github.com/OK-DMR/ok-dmrlib/blob/master/okdmr/dmrlib/hytera/pdu/radio_registration_service.py)
  and [hdap.py](https://github.com/OK-DMR/ok-dmrlib/blob/master/okdmr/dmrlib/hytera/pdu/hdap.py):
  RRS opcodes, reply layout and HDAP checksum.
- OK-DMR [gpsdata.ksy](https://github.com/OK-DMR/dmr-kaitai/blob/master/okdmr/kaitai/hytera/gpsdata.ksy):
  uncompressed Hytera GPS uses degrees and decimal minutes. It does not
  establish the compressed coordinate flags.

## Data-channel signalling follow-up

Further correlation identifies a narrow, observed data-control profile on the
corpus data carrier. These are capture-derived names, not a published
Hytera air-interface specification. Every decoded form retains its raw bytes;
other flag combinations and layouts fall back to raw vendor messages.

| Message | Fields established in this capture |
| --- | --- |
| CSBK `8D 68` with zero remaining fields | Repeated data-channel beacon; big-endian activity word at bytes 2..4 |
| Short LC `C`, embedded manufacturer `68` | Same activity word at bits 12..28 (MSB first) |
| Full LC `0D`/`8D`, terminator burst | Data setup: bit 7 names slot 0/1; source radio at bytes 7..9; activity word at bytes 3..5 |
| Full LC `30 68 F0`, terminator burst | Data end: source radio at bytes 6..8; activity word at bytes 3..5; trailing byte `60` retained |

Byte ranges exclude their end. A data-end radio ID is **one byte earlier** than
the normal voice-LC source; applying the voice layout to it produces a wrong
radio ID. The decoder only names the exact observed fixed-field profiles.
In particular, the setup's top bit is a slot indicator in this profile; that
interpretation is not extended to unrelated protected LC messages.

Checks against packet headers verify setup radio IDs and slots, and end radio
IDs including dispatcher replies. The short-LC activity high bit is exposed
as an experimental busy hint. Other activity bits and repeater mappings are
not established; no frequency is learned from this word.

CSBK `08` remains unidentified. Its flags and full payload stay raw; it does
not generate an acknowledgement, denial, grant or position. Compressed GPS
hemisphere/fix flags outside the observed profile and motion-word units also
remain unverified and are not guessed by the decoder.

### Protocol references

Hytera's *XPT System Product Description R3.0*, *XPT System Application Notes
R5.0*, and *XPT System White Paper R1.1* describe dedicated data repeaters,
GPS/RRS services and data revert. They support the architecture, but do not
specify the remaining compressed GPS flags or proprietary activity bits.
*Interfaces of Hytera Digital Radios v1.07* describes hardware interfaces,
not these air-interface layouts. These documents are not redistributed here.

### Public corpus validation

Only corpus samples containing Hytera traffic are listed below. No decoded
coordinates, radio-location descriptions or private IQ files are included.

| Corpus ID | Validation |
| --- | --- |
| `a7a6bb2cf35d069a` | XPT voice/signalling regression: decoded voice frame hashes match upstream |
| `b69007e66f75c602` | XPT voice/signalling regression: decoded voice frame hashes match upstream |
| `4aa34b203c13d715` | Compressed GPS, RRS requests/replies and data-control diagnostics decoded; original calls preserved |
| `45c4719b910f2af8` | Repeated compressed GPS and data-control diagnostics decoded; original calls preserved (not yet a regression test, so no expected output here) |

The following voice captures have been uploaded to the corpus and are
awaiting publication/public sample IDs:

| Pending corpus capture | Validation |
| --- | --- |
| `20261123-181352.799-voice-460287500` | Embedded DMR GPS decoded on Hytera XPT; attributed to the talking radio and retained in call JSON; voice frame hashes match upstream |
| `20261123-180841.063-voice-460287500` | Embedded DMR GPS decoded on Hytera XPT; attributed to the talking radio and retained in call JSON; voice frame hashes match upstream |

Tests use anonymous, generated coordinates for packet and embedded-LC vectors.
They cover all hemispheres for embedded GPS; compressed GPS remains limited
to the observed flag profile. Positions are checked through conventional and
trunked engine events and saved call JSON.

To replay a published voice sample through the call recorder, download its
files into a folder, then use its recorded channel rate and frequency:

```bash
trunk-pro replay --source SAMPLE/source-0.sigmf-data,460287500,32000,cf32 \
  --dmr 460287500 --guard 0 --out replay-output --keep-silent --record-encrypted
```

For the two pending samples above, each saved call JSON has a non-null
`lat`/`lon` on the source entry for the radio that sent GPS. Compare voice
frame hashes with the upstream binary using `tool corpus run`; its voice-tier
comparison checks voice frames rather than GPS output, so inspect call JSON
separately to verify the new position path.
