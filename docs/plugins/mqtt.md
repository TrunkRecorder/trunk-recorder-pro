# MQTT

This page covers the MQTT plugin: what it publishes, its settings, and the Home Assistant option.

The MQTT plugin publishes calls, radio activity and system status to an MQTT broker. Its messages
use the format of Trunk Recorder's MQTT Status plugin, so consumers of that (Home Assistant,
Node-RED, dashboards) read them unchanged.

| | |
|---|---|
| Plugin id | `mqtt` |
| Hears | calls starting and ending, recorded calls, radio activity, live system status |
| Audio | WAV; M4A when the recorder encodes it for another plugin |
| Source | [github.com/TrunkRecorder/trunk-plugin-mqtt](https://github.com/TrunkRecorder/trunk-plugin-mqtt) |

## Setting it up

1. On **Plugins** > **Install & settings**, find **MQTT** under **Find plugins** and press
   **Install** (or run `trunk-pro plugin install mqtt`). Use version 0.1.7 or later with
   Trunk Recorder Pro 0.2.0: an older one reports a call's signal, noise and frequency error as 0.
2. In **Setup** > **Plugins**, turn **MQTT** on, and fill in **Broker** (`tcp://host:1883`; `ssl://`,
   `ws://` and `wss://` work too) and **Topic**, the base topic for its messages.
3. Start recording, or if you're recording already, the plugin restarts with the new settings.

It works over MQTT 3.1.1 or 5. A slow or unreachable broker never holds up recording: messages wait
in a queue with an age and a size limit, and the oldest are dropped past them. The broker's state, the
queue and what has been sent are on the plugin's card on the Plugins page's Health tab.

## Settings

All for the whole recorder (Setup > **Plugins**, or `plugins.mqtt.settings` in `config.json`);
there are no per-system settings.

| Key | Default | Description |
|---|---|---|
| `broker` | `tcp://localhost:1883` | **Broker.** URL with protocol and port |
| `topic` | | **Topic.** Base topic for status messages. Required |
| `unitTopic` | | **Unit topic.** Base topic for radio activity, followed by the system's short name. Empty turns it off |
| `username`, `password` | | Broker login |
| `clientId` | `tr-status-<crc>` | Made from the broker and topic when empty |
| `instanceId` | `trunk-recorder` | Added to every message as `instance_id` |
| `qos` | `0` | 0, 1 or 2 |
| `mqttVersion` | `3` | `3` (3.1.1) or `5` |
| `mqttAudio` | `false` | Publish each recorded call's audio, base64-encoded, to `topic/audio` |
| `mqttAudioType` | `wav` | `wav`, `m4a`, `both` or `none` (metadata only) |
| `queueMaxAge` | `60` | Seconds a message may wait for the broker before it's dropped |
| `audioMaxAge` | `300` | The same, for audio |
| `queueMaxMb` | `32` | MB of waiting messages; past this, the oldest are dropped |
| `heartbeatInterval` | `10` | Seconds between pings and heartbeats (minimum 5); `0` turns them off |
| `heartbeatTimeout` | `30` | Seconds a connection attempt or send may take; raise it for audio over a slow uplink |
| `homeAssistant` | `false` | Publish Home Assistant's MQTT discovery messages |
| `discoveryPrefix` | `homeassistant` | The prefix Home Assistant's MQTT integration listens under |

Trunk Recorder's own key names (`unit_topic`, `mqtt_audio`, `queue_max_age`, …) are accepted too, so
its settings can be pasted in as they are. `m4a` sends no audio, and `both` sends WAV only, when no
M4A is made (see [M4A audio](README.md#m4a-audio)).

## What it publishes

Every message is JSON: `{"type": <subtopic>, <name>: <data>, "timestamp": <unix s>, "instance_id": <id>}`.
Under the base topic: `config` and `systems` (retained, sent again after each reconnect), `system`,
`rates` (every 5 s), `calls_active` (every second), `call_start`, `call_end`, `call_ignored`, `audio`,
and `trunk_recorder/status` and `heartbeat`. Under `unitTopic/<shortName>`: `call`, `end`, `on`,
`off`, `ackresp`, `join`, `data`, `ans_req`, `location`, `call_alert` and `emergency`. The plugin's
README has the full table and
[example messages](https://github.com/TrunkRecorder/trunk-plugin-mqtt/blob/main/example_messages.md).

Compared with Trunk Recorder's plugin, Trunk Recorder Pro adds `call_ignored` (a call that wasn't
recorded, and why), `emergency`, `snr` and `clean_voice_pct` in `call_end`, `decrypted`, `algid` and
`key_id` on calls, and a radio's `lat` and `lon`. It doesn't publish `recorders`, `recorder`, `console`
or the trunking message topic, and `rec_num`, `src_num` and `rec_state` in active calls are `-1`.

## Home Assistant

Turn on **Home Assistant discovery** and Home Assistant's MQTT integration adds a device for the
recorder with no YAML: whether the recorder is up, its active calls and last call, and for each system
with a control channel its decode rate, control channel and active calls. Nothing more is sent to the
broker for it. Discovery adds entities but doesn't remove them: after a system is renamed or removed,
delete the old device in Home Assistant.
