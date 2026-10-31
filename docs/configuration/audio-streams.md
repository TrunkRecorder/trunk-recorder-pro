# Internet audio streams

An internet audio stream (a scanner feed: MP3 or PCM WAV over HTTP or HTTPS) is a row of a
conventional system with mode `stream`: an address where a channel has a frequency. It needs no
radio. A system can be streams alone, or streams beside its radio channels.

In **Setup → Conventional systems**, press **Add a stream** in a system's channel list (or pick
**Internet stream** as a row's mode). Enter its address, which of a stereo stream's channels to
record (both mixed, left or right; a feed may carry one scanner on each), and the TG Number its
calls are filed under. A stream has no frequency to file them under otherwise, so the TG Number is
required.

Its audio makes calls as a channel's does:

- A call opens when the audio is louder than about −42 dBFS (the stream's stand-in for a
  carrier), with the conventional pre-roll of audio from before. The pauses between words are
  kept (half a second after the last loud audio); the call ends after the system's call timeout
  (`callTimeoutS`) with nothing loud.
- A call longer than the system's longest call (`maxCallS`) is saved and carries on in a new
  one, as a stuck carrier's would.
- The call is saved, played live, uploaded and passed to plugins by the system's rules, as any
  conventional call: its plugin settings and delays, its recording rules, its talkgroup names.
- The system's [signals](../guides/signals.md) are listened for in it: a two-tone page on a feed
  is named as one on the air would be.

A call's `voice_mode` is `stream` and its `freq` is 0.

**Its clock is its samples.** A stream's time runs by the audio received (8 kHz), not the wall
clock, so a stream that arrives slower or faster than the air keeps its own time. When it
connects, and while it's disconnected, silence fills in up to now: the time it was away passes,
and a call in progress ends. A stream that ends or fails is connected again every 2 s; why is in
the log.

The address may hold the feed's user and password. It is kept in the config file, and isn't sent
to listen-only [interfaces](../guides/interface.md). The desktop and server apps decode streams
themselves (no FFmpeg needed); the browser-only version has no streams.

```json
{
  "sources": [],
  "conventional": [
    {
      "shortName": "county",
      "channels": [
        { "freqHz": 0, "mode": "stream", "url": "https://user:pass@feeds.example/county.mp3", "stereo": "left", "talkgroup": 1, "name": "County Fire" }
      ]
    }
  ]
}
```

In a [channel file](conventional.md#channel-file-channelfile), a stream row has `stream` as its
Mode, an empty Frequency, and its address in a `URL` column (and `Stereo`, when not both).
