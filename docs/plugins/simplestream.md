# simplestream

This page covers the simplestream plugin: what it streams, its settings, the packet formats, and
how to set it up.

simplestream sends the audio of calls, live as they're recorded, to other programs over UDP or
TCP: a player, a mixer, or something that compresses it and streams it on. It does what Trunk
Recorder's simplestream plugin does, in the same packet formats, so programs written for that work
with this.

| | |
|---|---|
| Plugin id | `simplestream` |
| Hears | live audio, calls starting, calls ending |
| Audio | raw PCM: 16-bit, mono, little-endian, 8 kHz |
| Source | [github.com/TrunkRecorder/trunk-plugin-simplestream](https://github.com/TrunkRecorder/trunk-plugin-simplestream) |

The audio is uncompressed, about 128 kbit/s per call. That's fine on one computer or a home
network, but it isn't meant to go over the internet as it is. Analog calls are 8 kHz too.

simplestream doesn't take recorded calls, so it has no say in which files are kept or deleted.

## Setting it up

1. On **Plugins** > **Install & settings**, find **simplestream** under **Find plugins** and press
   **Install** (or run `trunk-pro plugin install simplestream`).
2. In **Setup** > **Plugins**, turn **simplestream** on and press **Add stream**.
3. Fill in **Send to** (`udp://127.0.0.1:9123`, say), the **Talkgroup** to stream (`0` for every
   talkgroup), and if you have more than one system, the **System**.
4. Add more streams for other talkgroups or destinations.
5. Start recording, or if you're recording already, the plugin restarts with the new settings. Its
   log lists each stream, like `streaming talkgroup 1039 of dcfd to udp://127.0.0.1:9123`.

The plugin won't start without at least one stream (*Add a stream: where to send audio, and which
talkgroup's.*), or with a **Send to** it can't read.

### Listening to a stream

The plugin's repository has `examples/example_audio_player.py` (from Trunk Recorder), which plays a
UDP stream. To play one through PulseAudio on Linux, load its TCP module:

```bash
pacmd load-module module-simple-protocol-tcp sink=1 playback=true port=9125 format=s16le rate=8000 channels=1
```

and set the stream's **Send to** to `tcp://127.0.0.1:9125`, with nothing before the audio (both
packet switches off).

## Settings

simplestream has settings for the whole recorder only: a list of streams
(`plugins.simplestream.settings.streams` in `config.json`). Each stream sends some of the audio to
one place.

| Key | Required | Default | Type | Description |
|---|---|---|---|---|
| `url` | ✓ | | string | **Send to.** `udp://host:port` or `tcp://host:port`. |
| `TGID` | | `0` | integer | **Talkgroup.** The talkgroup to stream; calls it's patched into count too. `0` streams every talkgroup. |
| `shortName` | | (every system) | string | **System.** The short name of the system to stream, chosen from a menu. Empty for every system. |
| `sendTGID` | | `false` | bool | **Talkgroup in each packet.** Put the talkgroup number before the audio in each packet. |
| `sendJSON` | | `false` | bool | **JSON in each packet.** Put the call's details before the audio in each packet, instead of the talkgroup. |
| `sendCallStart` | | `false` | bool | **Packets when calls start.** With JSON: also send a packet of JSON, with no audio, when a call starts. |
| `sendCallEnd` | | `false` | bool | **Packets when calls end.** With JSON: also send one when a call ends. |

```json
{
  "plugins": {
    "simplestream": {
      "enabled": true,
      "settings": {
        "streams": [
          { "url": "udp://127.0.0.1:9123", "TGID": 1039, "shortName": "dcfd" },
          { "url": "tcp://192.168.1.30:9124", "TGID": 0, "sendJSON": true, "sendCallStart": true, "sendCallEnd": true }
        ]
      }
    }
  }
}
```

Talkgroup numbers repeat between systems. If you stream more than one system, give each its own
port, or turn on **JSON in each packet**, which names the system.

Only calls that are being recorded are streamed.

## Packets

Each packet is one slice of a call's audio, with, depending on the stream's settings:

- **Nothing before it** (both switches off).
- **The talkgroup** (**Talkgroup in each packet**): 4 bytes, little-endian, then the audio.
- **JSON** (**JSON in each packet**): the JSON's length in 4 bytes, little-endian, then the JSON,
  then the audio:

  ```json
  {"audio_sample_rate":8000,"event":"audio","freq":857587500,"patched_talkgroups":[1039],
   "short_name":"dcfd","src":1234,"src_tag":"","talkgroup":1039}
  ```

With **Packets when calls start**, a call start is a JSON packet with no audio:
`"event":"call_start"`, with `src`, `src_tag`, `talkgroup`, `talkgroup_tag`,
`patched_talkgroups`, `patched_talkgroup_tags`, `freq` and `short_name`. With **Packets when calls
end**, a call end has `talkgroup`, `patched_talkgroups`, `freq`, `short_name` and
`"event":"call_end"`.

In an audio packet, `talkgroup` is the talkgroup the stream is for, which is the patched talkgroup
when the call is on another one. `src` is the radio heard last when the call started, or `-1` when
none was.

## When it can't send

Audio that can't be sent right away is dropped: it's live, and a slow or missing receiver never
holds up the plugin or the recorder.

- **TCP** streams connect when their first audio comes, and connect again after a connection
  breaks (every few seconds while the other end isn't there).
- While it can't send to a destination (a TCP receiver that isn't listening, say), the plugin
  shows **Needs attention** with which one and why, and goes back to running once it can send
  again. UDP has no connection, so a UDP receiver that isn't there usually goes unnoticed.

The **Health** tab lists each destination as up or down, and the bytes sent.

## Differences from Trunk Recorder

- With **Talkgroup** `0`, a patched call is sent once, not once for each talkgroup in the patch.
- `src_tag` is always empty.
- Calls end when the recorder ends them, a little before their files are written (where Trunk
  Recorder sent `call_end`).
- There's no `audioStreaming` setting to turn on: the recorder sends live audio to any plugin that
  asks for it.

## Coming from Trunk Recorder

**Import Trunk Recorder config…** brings over your simplestream `streams`: `TGID`, `shortName`,
`sendTGID`, `sendJSON`, `sendCallStart` and `sendCallEnd` as they were, and the older `address`,
`port` and `useTCP` turned into a **Send to** address (`udp://127.0.0.1:9123` when they're
missing). The plugin also reads `address`, `port` and `useTCP` if you paste a Trunk Recorder
stream into `config.json` as it is.
