# Vendored from freq-finder

`ff/` is a **copy** (not a shared package) of freq-finder's P25 and vocoder DSP,
taken from `robotastic/argusrf` (freq-finder) at commit `856f4c9`,
`web/src/`. Edit freely here; nothing syncs back.

| Here | freq-finder source |
|---|---|
| `ff/p25/*.ts` | `dsp/p25/` (c4fm, cqpsk, crc, encode, fec, modulation, nid, phase2, phase2Tables, trellis, tsbk, tuning, voice, voiceSim, voiceTables) |
| `ff/mbe/*.ts` | `dsp/mbe/` (mbelib port) |
| `ff/p25Voice.ts`, `ff/demod.ts`, `ff/filters.ts`, `ff/fft.ts` | `dsp/` |
| `ff/analysis.ts` | **trimmed** `dsp/analysis.ts`: only `stft` / `averageSpectrum` |
| `ff/workbench.ts` | **new**: the `DecodedMessage` type from `workbench/chains.ts` |
| `../sources/WebRtlSource.ts`, `SignalSource.ts`, `RTLSDRGains.ts` | `sdr/WebRtlSource.ts`, `sdr/SignalSource.ts`, `model/RTLSDRGains.ts` (import paths; the constructor's parameter property became a plain field so node can strip its types) |

Changes on copy:

- `p25Voice.ts` imports `DecodedMessage` from `./workbench.ts`.
- `p25Voice.ts` `P25LiveVoice` takes `acquireIntervalS` (default 2, freq-finder's
  hard-coded value). Trunked voice channels pass 0.25: they open with pre-roll
  that starts before the voice does, and a 2 s retry lost the first ~1.3 s of
  every call.

Licensing: the op25-derived files are GPLv3; `mbe/` is an mbelib port (ISC,
notice kept in `mbe/tables.ts`). IMBE/AMBE+2 are DVSI vocoders.
