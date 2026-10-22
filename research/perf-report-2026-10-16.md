# Performance report analysis: 2026-10-16 (Xeon, 5 sources, 39 NXDN carriers)

Source: `trunk-pro-perf-20261016-2039.json`, from the `stage-timing` branch (release build).

## Setup

| | |
|---|---|
| Machine | Intel Xeon E5-2620 v4 @ 2.1 GHz, 8 cores / 16 threads, 64 GB, Ubuntu 24.04 (kernel 6.8) |
| Sources | 2 × Airspy at 10 MS/s, 3 × RTL-SDR at 2.4 MS/s |
| Trunked systems | 4 P25 (QPSK), 5 NXDN Type-D (7–8 repeaters each, 39 carriers in all) |
| Conventional | 21 channels (17 NXDN48, 4 FM) |
| Recording | 32 recorders, `fixed` vocoder, 1 plugin (rdioscanner) |

## Summary

The engine thread is the bottleneck: it's at **99.9 % of one core**. Most of that time goes to
watching the NXDN control carriers. The rest of the machine is nearly idle.

## What the report shows

Figures are from the busiest minute; the last hour looks the same.

- **One core pinned, the rest idle.** The other 15 hardware threads average about 20 %. The
  whole process uses 2.5 of 16.
- **Samples are being lost.** They wait 0.5 s on average (up to 1.1 s) between the driver and the
  engine. Each Airspy drops about 28 million samples a minute, roughly 5 % of its stream, so calls
  on them lose audio. The RTLs show 0 drops, but an RTL can't report overflow, so they're likely
  losing samples too.
- The Linux per-thread CPU readings worked on real hardware. Timing overhead was 0.3 % of a core.

### Engine time by stage

| Stage | % of one core |
|---|---:|
| `control.nxdn` | **61.7** |
| `chz.fft` (channelizer forward FFT) | 19.2 |
| `session.push` (signal meter, mostly the 10 MS/s Airspys) | 3.4 |
| `chz.head` (per-channel inverse FFTs) | 3.2 |
| `control.p25` | 2.5 |
| P25 voice and vocoders | ~4.4 |
| Conventional, call logic, everything else | ~5 |

### Engine time by source

| Source | What it runs | % of one core |
|---|---|---:|
| 0: Airspy, 10 MS/s | 2 P25 control channels, up to 4 voice channels | 12.9 |
| 1: Airspy, 10 MS/s | 2 P25 control channels, up to 3 voice channels | 12.8 |
| 2: RTL-SDR, 2.4 MS/s | 20 NXDN carriers, 12 conventional | **35.3** |
| 3: RTL-SDR, 2.4 MS/s | 3 conventional | 2.3 |
| 4: RTL-SDR, 2.4 MS/s | 19 NXDN carriers, 3 conventional | **35.6** |

### Why NXDN costs so much

Every watched carrier runs a full C4FM receiver at the channelizer's output rate. On a 2.4 MS/s
source that's 37.5 kS/s: 15.6 samples per symbol for 2400-baud NXDN. That comes to 81 µs per
block per carrier, about **420 ns per sample**. A P25 control channel costs about 160 ns per
sample.

## What this means for splitting the engine

| Approach | Effect on the engine thread | Verdict |
|---|---|---|
| Channelizer only on per-source threads | Takes about 24 % off, leaving it near 75 % | Not enough: no headroom, and drops return when calls pick up |
| Channelizer plus each source's control and voice decode on per-source threads | Busiest thread about 36 % (RTLs 2 and 4); the engine keeps call logic, about 5 % | The architecture to build. What crosses threads is decoded messages and audio, which are tiny |

## A cheaper fix to do first

Making NXDN carrier watching cheaper would fix this user without any threading:

1. **Thin the samples before demodulation.** The channelizer can't decimate below 37.5 kS/s on a
   2.4 MS/s source. Decimating a further 2–4× inside the NXDN receiver should still leave plenty
   of samples per symbol.
2. **Skip idle repeaters.** Type-D repeaters are idle most of the time. Only demodulate a carrier
   while it has signal on it; the channelizer's band power is already computed.

Together these could plausibly bring `control.nxdn` from about 60 % down to 15–20 %, and the engine
to around 55 %. That's an estimate until it's measured on a new report from this user.

## Recommendation

1. Fix NXDN carrier watching first: it's local and testable against the NXDN test IQ.
2. Then build the per-source split, with decode on the source threads, for headroom.

## Side notes

- The Airspy driver threads (`source-0`, `source-1`) use about 40 % of a core each, on other
  cores. That's libairspy's own sample conversion, not the engine.
- The `history` thread used 60 % of a core during the busiest minute (1.9 % over the hour). It's
  the one-off scan of the recordings folder at startup.
