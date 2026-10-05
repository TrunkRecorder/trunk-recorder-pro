# NXDN: design, tests and what is still unknown

October 2026. Branch `nxdn`. Covers conventional NXDN (NXDN48 and
NXDN96), Type-C trunking (a control channel) and Type-D trunking (Icom IDAS
distributed). Sources: the NXDN Forum's technical specifications (Part 1-A
Common Air Interface v1.3, 1-B, 1-C, 1-D, 1-E v1.1 and 1-F, mirrored at
qsl.net/k/kb9mwr/projects/dv/nxdn/), checked against SDRTrunk's NXDN module
(July 2026), DSD-FME, MMDVMHost and dvmhost. The code is written from the
specs; the others were read for facts and cross-checks only.

## No NXDN here

A sweep on 2026-10-04 (dongles 91 and 200) found none: VHF 150.8–175, UHF
450–470, 800 (851–869) and 900 (935–941.6) MHz, 60 s per 2.4 MHz, plus 5
minutes at 451.0 MHz, where FleetCall's Towson, MD NEXEDGE site (control
channel 451.01875, the nearest NXDN RadioReference lists) would be. The
detector — the NXDN frame sync at both rates, at least 6 frames in a row —
found the sigidwiki recordings at once. RadioReference lists no NXDN in DC,
Northern Virginia, Montgomery or Prince George's.

So, as with DMR Tier III, testing is on recordings and synthesized signals:

| Material | What it checks |
|---|---|
| sigidwiki `NXDN_IQ.zip`: `NXDN48 IQ.wav` (11.8 s), `NXDN96 IQ.wav` (15.3 s), 48 kHz IQ | The receiver, framer, scrambler, LICH, SACCH, FACCH1, voice FEC, layer 3 on real radios |
| dsd-neo's fixtures (`tests/fixtures/iq/nxdn48*.iq`, `nxdn96.iq`, cu8 48 kHz, cut from the above) | The same, at another level |
| `nxdn::synth` | Everything the recordings don't have: CAC, Type-C grants, DFA, Type-D SCCH, the engine paths |

## What is built

```text
channel IQ → dsp::C4fm (baud 2400 / 4800, RRC α 0.2) → nxdn::frame::Framer
  (FSW −3 +1 −3 +3 −3 −3 +3 +3 −1 +3, either polarity; 192-symbol grid;
   a lock must be followed by a valid LICH) → descramble (PN9 from 0x0E4)
  → LICH (7 bits + parity, outer symbols only)
  → nxdn::channel: SACCH / SCCH (60), FACCH1 (144), UDCH / FACCH2 (348),
    CAC (300): de-interleave, de-puncture, K=5 soft Viterbi, CRC-6/7/12/15/16
  → nxdn::layer3: VCALL, TX_REL, VCALL_ASSGN(_DUP), SITE_INFO, SRV_INFO,
    CCH_INFO, ADJ_SITE_INFO, …
  → voice: 4 × 72-bit AMBE+2 codewords a frame — the same codeword FEC and
    bit order as DMR (crate::ambe::decode_vcw), so the same vocoder
```

- **Conventional** (`ConvMode::Nxdn(rate)`): `nxdn::voice::NxdnVoice` takes
  the group, source and cipher from VCALL (FACCH1 at the start, the SACCH
  superframe every 4 frames) or, on a Type-D traffic channel, the SCCH's
  INFO1–4; the RAN from the SACCH. Rows split a frequency by RAN and group.
  The detector band for NXDN48 is ±3.1 kHz (6.25 kHz channels).
- **Trunking** (`Protocol::Nxdn`, `nxdn::trunking::Site`): every listed
  frequency is watched at once, as for DMR.
  - Type-C: CAC messages become grants (VCALL_ASSGN) and updates (_DUP).
    Channel number → frequency: the config's table; Direct Frequency
    Assignment when SITE_INFO says so (base + number × step; grants then
    carry 16-bit frequency numbers and a bandwidth); or learned, when the
    granted group's VCALL shows up on a watched frequency within 4 s. Calls
    heard on a watched frequency are also reported there, so a site whose
    channel numbers are unknown still records them. SITE_INFO gives the
    system and site code (the site lock's `sysId` / `site`) and the RAN.
  - Type-D: no control channel. Each repeater's SCCH says idle (ID 2046),
    free repeaters, site ID, and during a call INFO1 (cipher), INFO2 / 4
    (destination: 5-bit prefix + 11-bit ID, G/U), INFO3 (source). A call is
    reported on the repeater carrying it.
- **Find my system**: each probe also runs both NXDN receivers; a CRC-valid
  CAC makes an `nxdnControl` candidate (with RAN, system, site), frames
  with a valid LICH an `nxdn` one.
- **Tools**: `tool nxdnscan`, `tool nxdn` (`--frames`, `--audio`,
  `--variant` receiver settings), `replay --nxdn48/--nxdn96/--nxdn-trunk`.

## Results

| | Frames | SACCH | FACCH1 | Voice codewords ≤ 1 error |
|---|---|---|---|---|
| sigidwiki NXDN48 (a conventional transmission, unit 901, RAN 1) | 129 | 129 / 129 | 2 / 2 | 512 / 512 |
| sigidwiki NXDN96 (unit 2, RAN 0; voice and FACCH1 frames alternate) | 358 | 358 / 358 | 358 / 358 | 716 / 716 |
| dsd-neo nxdn48, nxdn48_attenuated (the same 6 s cut) | 57 | 100 % | — | 228 / 228 |
| dsd-neo nxdn96 | 31 | 31 / 31 | 30 / 30 | 64 / 64 |

Through the whole engine (`replay --nxdn48` / `--nxdn96`) each recording is
one call with its unit and RAN. The NXDN48 recording's voice decodes
cleanly but is near silence (−77 dBFS): it seems to be a key-up with
nothing said; the NXDN96 one is speech.

Synthesized (`nxdn::synth`, all in `cargo test`): conventional calls at
both rates through the engine (whole audio, group, unit, RAN, JSON); rows
picked by RAN / group; a Type-C control channel and a traffic channel in one
band — grant followed, call recorded; channel numbers from the table, DFA
and learning; a Type-D site — the call found on the busy repeater from its
SCCH. The channel coding round-trips every channel with bit errors, and
matches the spec's worked example (CAC puncturing X4 / X12, interleave Y1,
Y13, Y25 …) and OP25's scrambler table.

Findings along the way:

- **The sigidwiki recordings are spectrally inverted.** Every symbol's sign
  is flipped; that is probably why GopherTrunk can't sync on them. The
  framer now locks on the FSW in either polarity and keeps it.
- **The spec's receive filter made things worse.** TS 1-A has the
  transmitter shape RRC × x/sin x and the receiver RRC × sin x/x. With the
  receiver's sin x/x (a one-symbol boxcar after the RRC, `--variant sinc`),
  clean voice codewords fell to 96.5 % / 91.5 % on the recordings, from
  100 %. These radios evidently don't pre-emphasise; the default is plain
  RRC. Worth trying on other radios.
- **The 4FSK receiver lost the first second of a keyed-up NXDN48
  transmission.** It decides symbols two timing blocks late (200 ms at
  2400 baud), judging each as quiet or not by the levels of its own time,
  so noise from just before the carrier came up went into the level
  history after the burst-start reset. Symbols now also count as quiet when
  they are 13 dB under the quietest of the last 0.1 s. It also starts
  slicing at the rate's own deviation (1050 / 2400 Hz) rather than P25's.
  The reference captures replay identically (every WAV, frame and message;
  the call JSON only gains `"ran"`).

## Not known, or not done

- **Direct Frequency Assignment** is in TS 1-A v2.0, which isn't public;
  the field layout is SDRTrunk's and DSD-FME's. Base code 4 is 750 MHz in
  DSD-FME and 450 MHz in SDRTrunk; this uses 750.
- **Type-C composite control channels** (RF channel type 3 on a Type-C
  site) are told from Type-D traffic by trying the CAC's CRC.
- **Type-D repeater numbers**: a carrier's own number is taken from INFO2
  during its calls; nothing uses the number → frequency table yet (every
  repeater is watched anyway).
- **Not decoded**: full-rate (EFR) voice (never seen on the air, per
  DSD-FME), data calls, encrypted voice (scrambler, DES, AES: marked and
  left out of the audio), Kenwood talker alias (PROP_FORM), inbound CAC.
- **Weak signals** are measured only on synthesized voice
  (`cargo test -p trunk-core --release nxdn_snr_curve -- --ignored
  --nocapture`; SNR in the channel's own bandwidth, voice codewords
  decoded as sent; the synthesizer's pulses are smoother than real RRC, so
  these are optimistic):

  | SNR | 20 | 15 | 12 | 10 | 8 | 6 | 4 dB |
  |---|---|---|---|---|---|---|---|
  | NXDN48 | 100 % | 99.5 | 96.9 | 91.2 | 76.4 | 44.2 | 9.2 |
  | NXDN48 without MSD | 100 | 99.2 | 95.9 | 85.5 | 63.8 | 23.0 | 1.6 |
  | NXDN96 | 99.4 | 96.6 | 89.7 | 78.8 | 64.7 | 45.6 | 17.2 |
  | NXDN96 without MSD | 99.7 | 96.9 | 89.7 | 79.1 | 61.6 | 29.4 | 3.8 |

  Multi-symbol detection (the DMR receiver's, at the NXDN baud) is on. A
  comparison with DSD-FME or SDRTrunk on the same real IQ is still to do.

## For testers with a real system

What helps most is a capture: 60–120 s of raw IQ with the system busy.

```bash
# RTL-SDR at 2.4 MS/s, centred so the control channel and some voice channels are inside
trunk-pro capture nxdn-451000000.cu8 --freq 451000000 --serial <SN> --seconds 120
# what is on it
trunk-pro tool nxdnscan nxdn-451000000.cu8 --center 451000000
# one channel's messages (a control channel: grants, SITE_INFO)
trunk-pro tool nxdn nxdn-451000000.cu8 --center 451000000 --freq 451018750 --nxdn 48 > cc.jsonl
# the whole recorder on it
trunk-pro replay nxdn-451000000.cu8 --center 451000000 --rate 2400000 \
  --cc 451018750 --nxdn-trunk typeC --nxdn-rate 48 --nxdn-channels 451118750,452381250 --messages --out calls/
```

Send: the `.cu8` (or its first 30 s), the `tool nxdn` output, the system's
RadioReference page (it lists the LCNs), and what the recorder got wrong.
For a Type-D site, the capture should cover several repeaters while calls
are on.
