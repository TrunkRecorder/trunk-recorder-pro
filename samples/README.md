# Sample recordings

17 calls (108 s of audio) recorded by Trunk Recorder Lite in Chrome, live from
an RTL-SDR (R820T, SN 202), 2026-09-29 07:56–08:01 local time.

- System: P25 Phase 1 simulcast (CQPSK/LSM), NAC 0x443, WACN 0xBEE00,
  SysID 0x445, control channel 857.9875 MHz. Source centered at 858.3 MHz,
  2.4 MSPS, gain 38.6 dB, ppm 0.
- Voice channels: 857.1875, 857.5875, 858.5875, 859.0375 MHz (Phase 1 FDMA).
- Talkgroups: 101, 728, 729, 1039, 3747, 12001. No talkgroup CSV was loaded,
  so the tag fields are empty.

Files use Trunk Recorder's naming, `<talkgroup>-<start epoch>_<freq Hz>`:

- `.wav`: 8 kHz mono 16-bit PCM, decoded IMBE voice. Only voice frames are
  written, so the gaps between transmissions are removed.
- `.json`: Trunk Recorder's call JSON. `srcList` has the radio IDs and their
  offsets (`pos`). `freqList[0].error_count` holds the vocoder frames that
  failed FEC.
- `index.ndjson`: the app's index, one line per call (all 17).

These were copied out of Chrome's OPFS storage for `http://localhost:5173`. In
the app, **Recent calls → Export to folder…** now does the same.
