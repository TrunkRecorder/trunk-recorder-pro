# Source advice: the best centre and sample rate for each source

November 2026. Issue #139 asked TRP to suggest centre frequencies and sample rates that cover
the configured systems, instead of a spreadsheet. Auto placement (#121) already places a source
whose centre is left blank. This plan adds advice for every source, a hand-set centre included,
and for sample rates, by building on the placement that exists rather than beside it.

## What existed

| Question | Answered by |
|---|---|
| Which source receives a frequency | `place::receiver` (placement, `Config::receiving`, Start, `listed_reached`) |
| Where an auto source goes | `place::place_sources` / `busiest_window` (Start, the first-run guide, the survey) |
| What's missed | three loops in `Config::problem` (a site's control channel, DMR / NXDN / EDACS frequencies, conventional channels), and `listed_reached` for the dashboard's "not needed" |
| The rates a radio runs at | per-kind lists in `web/src/config.ts` only; a radio's own `Caps.rates` in trunk-pro |
| What's heard but not covered | the radio registry's per-frequency calls and `out_of_band` counts |

So what has to be covered lived in Start's error messages, not in the targets placement works
from (placement took one conventional channel as enough, Start wanted all of them); Rust didn't
know what rates a source could run at; and placement saw only the frequencies the config lists.

## The plan

1. **Requirements in `Targets`, and one `coverage()`.** A `Group` says whether one of its needed
   frequencies is enough (a P25 or SmartNet site's control channels) or all are (a DMR, NXDN or
   EDACS site's control and watched frequencies, a conventional system's channels).
   `place::coverage` says, per group, whether its need is met and what's missed, and per source
   how many listed frequencies it's the first to reach. Start's checks, `Config::placement` and
   the dashboard's "not needed" read it.
2. **Sample rates in Rust.** `Rates` (a list or a range) moves to trunk-app, each `SourceKind`
   has its rates there (the web's lists read them through wasm), and one helper says whether a
   radio runs at a rate. Each kind may name a dependable top rate (an RTL-SDR's 2.56 MSPS):
   advice goes past it only when that covers more.
3. **`place::advise`.** A source to place also says whether its set centre may be advised on (a
   capture file's can't) and which rates it may run at. Advice re-runs `place_sources` with
   sources released to auto and rates tried in turn, and compares by, in order: requirements
   met, weight of channels reached, radios needed, rates past the dependable top, total sample
   rate (less CPU), how little changes. Only a strict improvement is advice.
4. **What's heard.** The targets take the registry's calls per frequency as weights, out-of-band
   ones included, so channels granted on the air count even when the config doesn't list them.
5. **Where it shows.** Setup's Tuning group per source and an Apply all for the sources; the
   dashboard's note; Start's errors say the fix when advice has one.

## Decisions (2026-11-07)

- A lower sample rate is suggested on every machine when it covers as much with as many radios:
  less CPU and USB traffic everywhere, not only on a Pi.
- **Use** sets a source back to auto when auto places it where the advice says (with the other
  sources as they are); otherwise it writes the suggested centre. A rate is always written.
