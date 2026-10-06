# Sources

Each entry in `sources` is one radio, or one capture file to replay. This page lists every source
type's keys. For choosing a sample rate, setting gain and ppm, profiling the guard band and running
several radios, see the [radios guide](../guides/radios.md).

```json
"sources": [
  { "type": "rtlsdr", "serial": "00000101", "centerHz": 0, "rateHz": 2400000, "gainDb": 25.4, "ppm": 1 },
  { "type": "airspy", "centerHz": 770500000, "rateHz": 6000000, "gainMode": "linearity", "gainStep": 14 }
]
```

A source receives a block of spectrum `rateHz` wide, centered on `centerHz`. Every source has a
`type`, one of `"rtlsdr"`, `"usrp"`, `"airspy"`, `"soapy"` or `"file"`. Any other `type` makes the file
unreadable.

## Keys every source shares

These work the same way on every type; each type's table below repeats them with that type's default.

### Center frequency and Auto

`centerHz` is the frequency in the middle of the block of spectrum the source receives, in Hz.
**`0` means Auto**: when recording starts, the app centers the source over whatever the sources
before it in the list don't cover yet. It tries, in order: every uncovered system's control channels
and known voice channels plus the conventional channels; just the control channels (and DMR and NXDN
watched frequencies) plus the conventional channels; the first uncovered system with its voice
channels; that system's control channels alone. It also keeps the center off any channel (the DC
spike at the center of an SDR's band). If none of those fit, recording won't start until you set a
center by hand.

Setup shows where an Auto source will be placed under **Center frequency, MHz**.

### Guard band and usable bandwidth

An SDR's anti-alias filter rolls off near the edges of its band, so signals there come in weak.
`guardHz` is how much is left unused at each edge, in Hz (default 75 000). A channel is used only if
it is within

```text
rateHz / 2 − guardHz
```

of the center, but never less than a quarter of the rate. At 2.4 MSPS with the default guard band,
that is 1.125 MHz either side of the center. **Profile roll-off…** next to **Guard band** in Setup
(and `trunk-pro rolloff`, see [command-line.md](../command-line.md)) measures where your radio's
noise floor sags and suggests a value.

If you're coming from Trunk Recorder: it left 32–64 kHz depending on the rate; the default here is a
little wider.

### Frequency correction (`ppm`) and AutoTune

`ppm` corrects the radio's frequency error, in parts per million. A frequency *f* is tuned as
*f* / (1 + ppm × 10⁻⁶). On an RTL-SDR it must be a whole number; on the other types a fraction is
fine.

The error is measured all the time on P25 and SmartNet control channels, and Setup shows it under the
ppm field with a button to set the correction. With `autoTune` on, the measured error (an average of
the last 20 measurements) is applied by itself: new voice channels open at the corrected frequency,
and a P25 control channel more than 150 Hz off is reopened at it (at most every 200 s).

## `"type": "rtlsdr"`

An RTL-SDR dongle, driven directly over USB (no driver library needed).

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `type` | ✓ | | string | `"rtlsdr"` |
| `serial` | ✓ | | string | The dongle's serial number. `""` = the first free dongle |
| `centerHz` | ✓ | | number | Center frequency, Hz. `0` = Auto |
| `rateHz` | ✓ | | number | Sample rate. 2 400 000 is typical; the interface offers 2 048 000, 2 560 000, 3 200 000, 1 920 000 and 1 024 000 too |
| `gainDb` | | `25.4` | number | Tuner gain, dB. Ignored with `agc`. Most dongles go 0–49.6 |
| `agc` | | `false` | bool | Use the tuner's AGC instead of `gainDb` |
| `ppm` | ✓ | | integer | Frequency correction, whole ppm |
| `autoTune` | | `false` | bool | Apply the measured frequency error by itself |
| `guardHz` | | `75000` | number | Left unused at each edge of the band, Hz |

The native driver handles R820T and R828D tuners (the RTL-SDR Blog V3 and V4, and most current
dongles). For an older dongle with an E4000, FC0012, FC0013 or FC2580 tuner, use a `soapy` source
with `"args": "driver=rtlsdr"`; see [other radios](../install/other-radios.md).

Two dongles with the same serial can't be told apart. Give each its own with `rtl_eeprom -s`.

## `"type": "usrp"`

An Ettus USRP, through UHD. UHD must be installed separately; the app loads it when needed.

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `type` | ✓ | | string | `"usrp"` |
| `args` | | `""` | string | UHD device arguments: `""` = the first found, `serial=…`, `type=b200`, `addr=192.168.10.2`. `num_recv_frames=256` is added unless you set it (UHD's default buffer overflows at high rates) |
| `centerHz` | ✓ | | number | Center frequency, Hz. `0` = Auto |
| `rateHz` | ✓ | | number | Any rate the device's clock supports, e.g. 8 000 000 |
| `gainDb` | | `40` | number | Gain, dB. B200/B210: 0–76 |
| `agc` | | `false` | bool | The device's AGC instead of `gainDb` (B200, B210 and E3xx only) |
| `antenna` | | `""` | string | e.g. `RX2`, `TX/RX`. `""` = the device's default |
| `ppm` | | `0` | number | Frequency correction, ppm |
| `autoTune` | | `false` | bool | Apply the measured frequency error by itself |
| `guardHz` | | `75000` | number | Left unused at each edge of the band, Hz |

## `"type": "airspy"`

An Airspy R2 or Mini, through libairspy. libairspy must be installed separately.

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `type` | ✓ | | string | `"airspy"` |
| `serial` | | `""` | string | Serial number in hex. `""` = the first found |
| `centerHz` | ✓ | | number | Center frequency, Hz. `0` = Auto |
| `rateHz` | ✓ | | number | R2: 10 000 000 or 2 500 000. Mini: 6 000 000 or 3 000 000 |
| `gainMode` | | `"linearity"` | string | `"linearity"` (best near transmitters), `"sensitivity"` (best for weak signals) or `"manual"` (each stage set separately) |
| `gainStep` | | `14` | integer | Linearity or sensitivity step, 0–21 |
| `lnaStep` | | `10` | integer | Manual mode: LNA step, 0–14 |
| `mixerStep` | | `10` | integer | Manual mode: mixer step, 0–15 |
| `vgaStep` | | `10` | integer | Manual mode: VGA (IF) step, 0–15 |
| `agc` | | `false` | bool | Manual mode: the Airspy's AGC sets the LNA and mixer (the VGA step still applies) |
| `biasTee` | | `false` | bool | Power an LNA through the antenna cable |
| `ppm` | | `0` | number | Frequency correction, ppm |
| `autoTune` | | `false` | bool | Apply the measured frequency error by itself |
| `guardHz` | | `75000` | number | Left unused at each edge of the band, Hz |

The Airspy gains are the driver's steps, not dB.

## `"type": "soapy"`

Any SDR with a SoapySDR module: HackRF, SDRplay, LimeSDR, PlutoSDR and others. SoapySDR and the
device's module must be installed separately.

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `type` | ✓ | | string | `"soapy"` |
| `args` | | `""` | string | Device arguments: `""` = the first found, e.g. `driver=hackrf`, `driver=sdrplay,serial=…`, `driver=rtlsdr` |
| `centerHz` | ✓ | | number | Center frequency, Hz. `0` = Auto |
| `rateHz` | ✓ | | number | A rate the device supports |
| `agc` | | `true` | bool | The device's AGC. Turn it off to use `gainDb` and `gains` |
| `gainDb` | | `null` | number or null | Overall gain, dB. `null` leaves it as the device has it |
| `gains` | | `{}` | object | Gain stages by name, dB, applied after `gainDb`: e.g. HackRF `{ "LNA": 32, "VGA": 20, "AMP": 0 }`, SDRplay `{ "IFGR": 40, "RFGR": 2 }` |
| `antenna` | | `""` | string | `""` = the device's default |
| `settings` | | `""` | string | Device settings as `key=value,key=value`, e.g. `biastee=true` (see `SoapySDRUtil --probe`) |
| `ppm` | | `0` | number | Frequency correction, ppm |
| `autoTune` | | `false` | bool | Apply the measured frequency error by itself |
| `guardHz` | | `75000` | number | Left unused at each edge of the band, Hz |

## `"type": "file"`

A capture on this computer, replayed as if it were a radio: for testing, or for recording a system
from an IQ file someone sent you.

| Key | Required | Default | Type | Description |
|---|:---:|---|---|---|
| `type` | ✓ | | string | `"file"` |
| `path` | ✓ | | string | The capture file. Recording won't start while it's empty |
| `centerHz` | ✓ | | number | The frequency it was recorded at, Hz |
| `rateHz` | ✓ | | number | Its sample rate |
| `realtime` | ✓ | | bool | `true`: play it at the speed it was recorded. `false`: as fast as the computer can |
| `format` | | from the name | string | `"cu8"` (rtl_sdr, unsigned 8-bit), `"cs16"` (signed 16-bit) or `"cf32"` (32-bit float: GNU Radio, UHD). Left out: `.cs16` and `.sc16` files are `cs16`; `.cf32`, `.fc32`, `.cfile` and `.complex` are `cf32`; anything else is `cu8` |
| `autoTune` | | `false` | bool | Apply the measured frequency error by itself |
| `guardHz` | | `75000` | number | Left unused at each edge of the band, Hz |

A `file` source has no gain or `ppm`. In the browser version, the file is chosen with
**Choose file…** and must be rtl_sdr output; it is forgotten when the page reloads.

`trunk-pro replay` plays a capture without changing the config; see
[command-line.md](../command-line.md).
