# Internet audio streams

An HTTP or HTTPS audio stream is an independent call system, recorded as silence-delimited calls
without a radio or conventional system. The recorder needs **ffmpeg** on `PATH` to decode the stream; the native
desktop/server app supports this source, not the browser-only build.

In **Setup → Conventional systems**, choose **Add internet audio source**, enter the URL, set its
system short name and talkgroup, and set how long silence ends a call. A 0.5-second pre-roll is
kept before detected audio. Activity is detected at approximately −42 dBFS; the default silence
hang is 3 seconds (configurable from 0.25 to 30 seconds). When the stream disconnects, an active
call is finalized and the source reconnects.
Calls are split into files after 120 seconds by default. After 600 seconds of continuous audio
(five default-size files), recording pauses until the configured silence timeout is reached;
then detection resumes. Both duration limits can be changed per stream in Setup.

Calls follow the normal recording finalizer: WAV and JSON files, the stream system's recording
rules, live audio subscribers, and call-concluded plugins. Configure plugin settings, delays and
signals in the stream's own Plugins section. The stream has no RF frequency, so its call records
use `freq: 0`.

The equivalent configuration is:

```json
{
  "sources": [],
  "audioStreams": [
    {
      "url": "https://provider.example/feed.mp3",
      "shortName": "dispatch_stream",
      "talkgroup": 1,
      "channel": "mix",
      "enabled": true,
      "silenceTimeoutS": 3,
      "maxCallS": 120,
      "maxContinuousS": 600
    }
  ]
}
```

The URL may include provider authentication. It is stored in the config file, passed to ffmpeg
through its standard input (not its command-line arguments), and hidden from listen-only interfaces.
Protect the config file and only grant control access to trusted users.

Set `enabled` to `false` to keep a stream configured without starting it.
