# Trunk Recorder Lite

Trunk Recorder in a web page. Plug an RTL-SDR into Chrome, enter the control
channels, press Start: it follows a **P25** trunked system (Phase 1 control
channel; Phase 1 and Phase 2 voice) and records every call it can hear to the
browser's storage as WAV + Trunk Recorder–compatible JSON. Nothing to install.

Status: working end to end on real air — see [Verified](#verified). Design and
evidence: [FEASIBILITY.md](FEASIBILITY.md).

## Run it

Needs Node 20+ and Chrome or Edge (WebUSB).

```bash
npm install
npm run dev        # http://localhost:5173
npm test           # 21 node tests: DSP, parser, call manager, end-to-end synthetic system
npm run build      # static site in dist/
```

The page must be **cross-origin isolated** (it shares sample buffers between
workers with `SharedArrayBuffer`). Vite's dev/preview servers send the headers;
for production, `public/_headers` sets them on Cloudflare Pages / Netlify. Any
other host needs:

```
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
```

### Without a dongle

Replay an `rtl_sdr` capture in the page (Source → Replay a capture), or
headless in node, which writes the calls to disk:

```bash
rtl_sdr -f 858300000 -s 2400000 -g 38.6 -n 48000000 capture.cu8   # 20 s
npm run replay -- capture.cu8 --center 858300000 --rate 2400000 \
    --cc 857987500,858987500 --out calls/ [--talkgroups tg.csv] [--verbose]
```

### Dongle setup per OS

- **macOS**: works as-is.
- **Windows**: install WinUSB for the dongle with Zadig (same as every RTL-SDR app).
- **Linux**: unload/blacklist `dvb_usb_rtl28xxu` and add a udev rule for USB `0bda:2838`.
- **Android**: Chrome + a USB-OTG adapter.

Recording stops if the tab closes or the computer sleeps. The page holds a
screen wake lock while visible; for long unattended runs, set the OS not to
sleep. If the USB link stalls (e.g. across a sleep/wake), the radio worker
reopens the dongle and carries on.

## How it works

```
main thread (React UI)  ── setup, status, waterfall, calls, live audio
   │
   ├─ radio worker ── WebUSB (4 transfers in flight) → u8 IQ
   │                  → shared fast-convolution channelizer (1 FFT per block, N channels)
   │                  → per-channel SampleRing (SharedArrayBuffer, fixed size, drop+count on overflow)
   │
   └─ trunk worker ── control channel → TSBK parser → CallManager
                      voice channels  → P25 voice decoder → per-call audio
                      concluded calls → WAV + JSON in OPFS (calls/<system>/<date>/)
```

| Path | What |
|---|---|
| `src/engine/channelizer.ts` | Overlap-save multi-head channelizer (after CyberEther's `filter_engine`); spectrum history for **pre-roll** — a voice channel opened by a grant starts ~1 s *before* the grant |
| `src/engine/ring.ts` | SPSC `SharedArrayBuffer` ring (after CyberEther's `CircularBuffer`) |
| `src/protocols/types.ts` | **The protocol seam**: `TrunkMessage` (Trunk Recorder's), `ControlDecoder`, `VoiceDecoder`, `ProtocolDriver` |
| `src/protocols/p25/` | P25 driver: streaming control-channel decoder, `tsbkParser.ts` (port of Trunk Recorder's `p25_parser.cc`), voice adapter |
| `src/protocols/registry.ts` | System types by name — add SmartNet / DMR / conventional here |
| `src/trunking/callManager.ts` | Call lifecycle, from Trunk Recorder's `monitor_systems.cc` |
| `src/trunking/trunkEngine.ts` | One system end to end; talks to the radio only through `ChannelPort`, so it runs in the worker and in node alike |
| `src/recording/` | WAV, Trunk Recorder call JSON, OPFS store |
| `src/vendor/ff/` | P25 + vocoder DSP **copied** from freq-finder (op25 / mbelib ports) — see `src/vendor/VENDORED.md` |
| `tools/replay.ts` | Headless replay of a capture |
| `research/bench/` | The benchmarks behind the design |

### Adding a system type

Implement `ProtocolDriver` (`src/protocols/types.ts`) and register it in
`src/protocols/registry.ts`. A trunked protocol supplies a `ControlDecoder`
that emits `TrunkMessage`s (grant / update / …) and a `VoiceDecoder` per voice
channel; the call manager, recorder pool, storage and UI need no changes.
Conventional systems will skip the control decoder and keep permanent voice
channels whose own activity opens and closes calls.

## Verified

On real air (a P25 Phase 1 simulcast/CQPSK site, NAC 0x443, WACN 0xBEE00,
SysID 0x445), from an R820T RTL-SDR at 2.4 MSPS:

- **In Chrome** (headless, replaying a 20 s capture of that site through the
  real workers, rings and OPFS): control channel 98–100 % of TSBKs decoded,
  CQPSK auto-detected, identity complete; two clear calls recorded (TG 2501
  19.3 s, TG 101 15.8 s) and played back from OPFS; encrypted and out-of-band
  grants monitored, not recorded; radio worker ≈ 9 % of a core.
- **Live from the dongle** (node + node-usb's WebUSB, same `RtlSource` /
  webrtlsdr driver, channelizer and engine, 120 s): 100 % of real-time
  samples received, longest USB gap 66 ms; control channel 92 % decoded
  (4106 TSBKs); 34 grants followed, clear calls recorded (TG 2501 45 s,
  TG 101 2 s); everything on one thread at 25 % of a core.
- **Tests** (`npm test`): channelizer gain/rejection/pre-roll exactness, ring
  wrap and overflow, TSBK field layout vs `p25_parser.cc`, call lifecycle,
  talkgroup CSV, and a synthetic P25 system end to end (grant → pre-rolled
  voice channel → complete call).

Not yet verified: WebUSB with a real dongle *inside Chrome* (automation can't
click the WebUSB chooser — do it by hand once: `npm run dev`, Source → RTL-SDR
dongle → Start); a Phase 2 call on real air; a 24 h run.

## Not in Lite (yet)

Uploads (rdio-scanner, OpenMHz, Broadcastify — planned, no server), SmartNet,
DMR, conventional systems, multiple dongles/systems, m4a/Opus encoding (WAV
only), MBT (multi-block) control messages, unit tags, and anything needing a
socket or a shell (simplestream, unit_script, stat_socket).

## License

GPL-3.0-or-later (the P25 code derives from op25). The vocoder is an mbelib
port (ISC). `@jtarrio/webrtlsdr` is Apache-2.0.
