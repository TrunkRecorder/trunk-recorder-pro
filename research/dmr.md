# DMR: design and first results

September 2026. Branch `dmr`. Covers conventional DMR (built) and DMR trunking
(designed, not built). Sources: ETSI TS 102 361-1/-4, SDRTrunk's and DSD-FME's
decoders, and Trunk Recorder's fork (`dmr_parser.cc`, `dmr_recorder_impl.cc`).

## What is built

- `trunk_core::dmr` decodes one carrier into both slots' link control, CSBKs and voice:
  - `burst`: sync search and a framer that holds the 30 ms grid between syncs,
    plus the CACH/TACT (which slot), slot type and EMB.
  - `fec`: Hamming(7,4), Golay(20,8), QR(16,7), BPTC(196,96), embedded LC,
    Reed-Solomon(12,9) and the masked CRCs.
  - `slot`: bursts become voice codewords, full LC (header, embedded,
    terminator), CSBK/MBC and PI header. `voice`: per-slot talkgroup, source
    and cipher, plus audio.
- Conventional channels take `"mode": "dmr"`, in the config, the channel CSV,
  the Setup page and `replay --dmr`. Each slot records its own calls. The call
  JSON gets `color_code`, and the file name gets `.<slot>` as on Phase 2.
- `trunk-pro tool dmrscan <capture> --center --rate` finds every DMR carrier in
  a capture. `tool dmr … --freq [--bursts] [--audio --slot]` decodes one.
- Reused unchanged:
  - The C4FM receiver: DMR is the same 4800-baud 4FSK with the same dibit mapping.
  - P25 Phase 2's AMBE codeword FEC (`decode_vcw`): DMR's AMBE interleave, DSD's
    `rW/rX/rY/rZ`, is identical to `VCW_MAP`.
  - The AMBE+2 vocoder.

Checked live from 90 s captures (dongle 91, 28 dB):

| Carrier | CC | What | Result |
|---|---|---|---|
| 463.5500 | 1 | conventional | TG 25 from 470, 100 % of AMBE codewords with ≤1 error |
| 464.4625 | 13 | conventional | TG 44772 from 1 and 4477084, 97.8 % clean |
| 463.3750 / 452.3625 | 14 | **Linked Capacity Plus** rest channel | TG 111 / 106 calls; weak here (32 % clean) |
| 463.7500 | 10 | Capacity Plus rest channel | site status, no traffic in 90 s |
| 464.3500 | 6 | Capacity Plus rest channel | site status, no traffic in 90 s |
| 452.1750 | 0 | **Capacity Max** control channel | ALOHA and C_BCAST readable; CRCs keyed |
| 463.4750 | 4 | conventional + data | slot 1 carries data and Motorola CSBK 0x29 |

So DMR trunking can be tested here after all: Capacity Plus live, and Capacity
Max on its control channel. Only Tier III and Connect Plus have to be tested
from synthetic signals or recordings.

Cost: one carrier (both slots, 90 s of air) decodes in 0.7 s including a whole
2.4 MHz channelizer. `dmrscan` runs 345 carriers at 0.06 % of a core each.

## Both slots: one front end, a recorder per slot

A DMR carrier is one receiver's work, whichever slots are recorded:

- Which slot a burst belongs to is known only from the demodulated stream: the
  CACH on a repeater, the sync pattern in direct mode. So a receiver for one
  slot has to demodulate and frame the whole carrier anyway.
- Voice bursts B–F carry no sync. The framer keeps the grid from syncs on
  *either* slot (the other slot's idle and data bursts all carry one), so a
  shared framer holds the grid better than one watching a single slot.

The recorder is still per slot. That is how the engine already treats P25
Phase 2: `Channel` is keyed by (system, frequency) and holds
`calls: [Option<CallId>; 2]`, a recording (a pool slot) is per call, and the
head closes when both slots are free. DMR voice channels drop into that
unchanged. Conventional channels now keep a live call per slot the same way.
Each slot has its own vocoder, LC state, timeout and file.

Your intuition that a recorder per slot is cheap enough is right. At 0.06 % of
a core per carrier, CPU doesn't decide it either way. The shared front end
wins on grid holding, not cost.

## Trunking variants

| System | How a call gets a channel | Channel numbers → frequency | Testable here |
|---|---|---|---|
| Capacity Plus / Linked Cap+ (Motorola) | No grants. Radios idle on a **rest channel**, which moves. CSBK 0x3E Site Status lists busy logical slots (LSN) with the low 8 bits of each talkgroup. The voice LC names the real talkgroup. | LSN = 2·(repeater−1)+slot+1; not on the air | Yes (3 sites) |
| Connect Plus (Motorola) | CSBK 0x03 grants: source, group, LSN (5 bits) | Not on the air | No |
| Capacity Max (Motorola, Tier III based) | Tier III grants plus Motorola 0x21/0x22 voice-channel updates; CRCs keyed (RAS) | Tier III channel numbers | Control channel yes |
| Tier III (ETSI) | CSBK 0x30–0x32 grants: 12-bit channel, slot, target, source | Absolute frequencies in MBC grants, or C_BCAST channel announcements | No |
| Hytera XPT | Short LC names the free repeater; LC 0x09 grants | Not on the air | Unknown |

Trunk Recorder's fork, for comparison:

- Its Tier III grant parser reads target and source from the channel-number
  bits (grant fields are channel 16–27, slot 28, target 32–55, source 56–79).
- It marks a call encrypted on service option `0x80`, which is Emergency;
  Privacy is `0x40`.
- It maps unknown LCNs to "the next unused frequency in the list" by guessing.

### What was built (trunking)

`dmr::trunking::Site` watches every listed frequency (a DMR receiver per
carrier) and turns what they carry into `trunk::Message`s for the existing
`CallManager`. Voice runs on `Voice::Dmr` channels (one head per carrier,
calls on its two slots, vocoding only the slots that record).

- **Capacity Plus.** Voice LC on any watched carrier and slot becomes a grant
  (and, every second, an update). Site status gives the rest channel and the
  busy slots. A busy repeater's idle slot also sends site status, naming the
  rest channel elsewhere, and so can a repeater in hang time. So a carrier is
  taken as the rest channel only once it has named itself on both slots for
  3 s. Result on the 90 s capture: TG 111 (5.5 s, radios 7011 and 7022) and
  TG 106, correct slots and colour code, repeater 1 = 463.375 learned.
- **Capacity Max.** The Tier III grant (0x31) and Motorola advantage-mode
  updates (0x22) name channel 101 slot 2. With 452.275 listed as a voice
  frequency, channel 101 = 452.275 is learned 2 s after the first update, and
  the call (TG 206, radio 2023, encrypted) is recorded. The control channel
  and the voice channel's LCs both have keyed checksums. Once a slot sees 3
  flawless BPTC blocks in a row fail their CRC, the whole site is treated as
  keyed, and blocks are taken with ≤3 BPTC corrections.
- **Tier III / Connect Plus.** Grants, absolute-parameter (MBC) grants and
  C_BCAST channel announcements are parsed. They are tested from spec
  layouts, since nothing on the air here uses them.
- **Colour code.** The site's is the first control block's on a listed
  control channel (or `colorCode`). Other colour codes are ignored; there is
  a neighbouring Cap+ repeater on 452.100.
- **Soft combining.** The embedded LC repeats every superframe, and headers,
  terminators and control blocks repeat too. When one copy fails, up to 4
  copies' soft bits are summed and tried again. Only a real CRC/RS pass is
  accepted this way, never a keyed-mode BPTC-only one. On weak 463.375:
  control blocks 801 → 1088, embedded LCs 3 → 6, voice headers 1 → 3. The
  call's start is found 6.6 s earlier.

Live (`trunk-pro serve`, dongle 91 at 464.2 MHz / 28 dB, about 10 minutes):

- Three Capacity Plus sites and four conventional DMR channels, at 3.7 % of
  a core in total.
- Each site learned its rest repeater within seconds: CC 14 repeater 1 =
  463.375, CC 10 repeater 1 = 463.750, CC 6 repeater 3 = 464.350.
- The maps are saved every 10 s while recording, not only when a session is
  stopped from the interface.
- CC 14 recorded TG 101 calls (radios 1040, 64250), and the conventional
  464.4625 TG 44772 (radios 2001, 1).
- Site status is logged only when it changes (at most every 10 s): about 20
  lines in 90 s instead of about 900.

End-to-end tests (`dmr::synth`) build a 4FSK repeater signal (idle, LC
headers, voice superframes with embedded LC, terminators) and check it
through both paths: a conventional DMR channel, and a trunked site.

## Still to do

- Weak signals: the receiver itself (voice syncs miss 5–7 of 48 bits on
  463.375). Worth an A/B test against DSD-FME on the same IQ, and a look at
  diversity as on P25.
- Capacity Plus: the site status only has talkgroups' low 8 bits, so a call
  is recorded from its link control. A repeater that isn't listed isn't
  heard, though the busy list says it exists: say so on the dashboard.
- Hytera XPT, and Capacity Max's other messages (registration, data).
- The dashboard shows a DMR site as a control channel. Show the variant, the
  rest channel and the learned channel table.
- Mobile / direct mode (simplex): bursty, so the C4FM receiver's rails and
  timing, set over ~0.5 s, see noise between bursts. Not tested.
- Only one colour code or slot per channel (a config field), talker alias,
  GPS.
