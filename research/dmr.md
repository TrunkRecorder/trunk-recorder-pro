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

### Proposed approach

1. **Capacity Plus first**, since it is live here. Treat the rest channel as
   the control channel and follow it as it moves. Open a voice head for each
   LSN that Site Status marks busy. Take the talkgroup from the voice LC,
   because Site Status only has its low 8 bits.
2. **Learn the LSN → frequency map instead of asking for it**, as SmartNet's
   band plan is learned. The channelizer sees every repeater of a site at
   once:
   - When the rest LSN is announced, the carrier we are on is that LSN's
     repeater.
   - When Site Status marks LSN *k* busy with talkgroup low byte *x*, the
     same-colour-code carrier whose slot just started an LC to a talkgroup
     ending in *x* is LSN *k*.
   - A few calls fill the table, and it is saved like the band plan. A
     configured table (TR's `lcnTable`) takes precedence.
   - Until the table is complete, energy detection on the site's carriers
     (the conventional DMR path) still records every call. This is also the
     fallback that makes Cap+/XPT usable with nothing but a frequency list.
3. **Tier III / Capacity Max grants.** CSBK → `trunk::Message::Grant` with
   `tdma_slot`, straight into the existing `CallManager`. Frequencies come from
   absolute channel parameters or C_BCAST; the configured table otherwise.
   Keyed CRCs: accept blocks whose BPTC is clean and whose residual recurs for
   that opcode, as SDRTrunk does.
4. **Engine**: `Voice::Dmr` beside `Voice::Tdma`, and a `SystemConfig::dmr`
   beside `smartnet`. Call ids, slots, pre-roll and the recorder pool stay as
   they are.

## Still to do on the conventional side

- Weak signals: 463.375 lost 0.6 s at a call's start. Late entry (joining
  between voice syncs) is strict so that noise doesn't make calls, and the
  receiver's outer threshold (voice syncs miss 5–7 of 48 bits there) is
  worth an A/B test against DSD-FME on the same IQ.
- Mobile / direct mode (simplex): bursty, so the C4FM receiver's rails and
  timing, set over ~0.5 s, see noise between bursts. Not tested.
- Only one colour code or slot per channel (a config field), talker alias,
  GPS.
- Vocode only the slots that have a call.
