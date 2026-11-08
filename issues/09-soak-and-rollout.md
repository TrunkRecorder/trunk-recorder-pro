# 09: Comparison, soak and decision

**Size:** S, plus elapsed time. **Needs:** 08. **Stop point:** keep or remove.

## Goal

Decide from measurements whether to keep this, compared with the issue 00 baseline.

## Work

1. Run both radios channelized for at least a week.
2. For a fair side-by-side, split one antenna to two radios, one in each mode, if a splitter is available.
   Otherwise alternate modes over similar days, as `docs/performance.md` does.
3. Measure the same list as issue 00, and add USB throughput per radio.
4. Check how often the 64-bin limit evicted a bin, and how many calls started with no pre-roll.
5. Write up the results in `research/fpga-channelizer.md`.

## Targets

| Measure | Target |
|---|---|
| Calls recorded | At least 99 % of wideband on the same air |
| `clean_voice_pct` | Within 1 point of wideband |
| Control channel good messages | At least 99 % of wideband |
| Dropped samples | No worse than wideband over USB3 |
| USB throughput per radio | 5.4 MB/s or less (42 bins) |
| Process memory | Clearly lower than the baseline |
| CPU | Lower than the baseline. Record the actual saving |
| Calls missing pre-roll | Rare after the first day |

## Decide

- **Keep:** if quality targets hold and the savings are worth maintaining two images, a Quartus toolchain and a
  second code path. Document the build and rebuild steps.
- **Remove:** if quality misses, or if the savings don't justify the upkeep. Delete `fpga/`, the backend and the
  config option, and keep the write-up.

Quality targets outrank the savings: don't keep a version that records worse, however much CPU it saves.
