# Signals

Dispatch sends more than speech: a two-tone page to set off a station's pagers, a long alert tone,
a hi-low warble, a DTMF or KNOX string to start a siren, a repeater's Morse ID. The recorder finds
these in every call's voice, keeps them in the call's JSON, and lists them on each system's page.
A pattern you give a name to is a **signal**: calls that carry it say so, and plugins are told it
by name the moment it's heard.

## Where tones come from

| Voice | How tones are found |
|---|---|
| P25 Phase 2, DMR, NXDN | The radio often sends a tone as tone frames, which name it exactly (to the nearest 31.25 Hz) and its level |
| P25 Phase 1, analog FM | The tone is in the sound itself, and measured (to a few tenths of a percent) |

Half-rate radios that don't send tone frames are measured like Phase 1. A tone found in the audio
has to hold steady for a while before it counts (a tenth of a second for a key, more for a lone
beep), so speech is rarely taken for one. The same detectors pick up an analog call's unit IDs
(MDC1200, FleetSync) and its CTCSS tone or DCS code: see [Tones and access codes](tones.md).

## What the tones make up

Each call's tones are put together into patterns:

| Pattern | What it is |
|---|---|
| `two_tone` | A page: tone A, then tone B. Its timing names its format (`Quick Call II` 1 s + 3 s, `Quick Call I`, `GE Type 99`, …) |
| `long` | One tone held a second or more: an alert tone |
| `hi_low` | Two tones taking turns: a warble |
| `pulsed` | One tone on and off |
| `digits` | A DTMF or KNOX string |
| `morse` | A CW ID, as text |
| `sequence` | A run of short tones: selective calling, a chirp |
| `beep` | A short tone on its own |

For a two-tone page, the recorder also suggests which paging codes the tones may be, likeliest
first: Motorola's Quick Call II pairs (with the pager code when its encoding plan pairs them), then
other plans' tones. Tones that no plan has are "custom tones". A pattern is told about a second
after its last tone: a page a second after its B tone ends, a DTMF string or Morse ID a second after
its last digit or letter. A call heard on several sites of one system is told once, from the copy
whose audio is played. The call's JSON has the same patterns once it's saved.

## The Tones card

At the bottom of each system's page on the **Radio system** tab, the **Tones** card lists every
pattern heard in the span you've chosen: which talkgroup carried it, its tones and timing, the
codes it may be, how often, when last, which radios sent it, and the signals it is. ▶ plays it in
the browser. **Add** makes a pattern a signal: give it an id (`TOWN_FIRE`) and a name, and choose
whether it counts only on that talkgroup.

Beeps are hidden unless you ask for them (**With beeps**): most are speech or background noise.

## Editing signals

A system's signals are in Setup, under the system's **Signals** (a conventional system's too). Type
one in from a paging list: each kind asks for what it is, a page's tone A and tone B, a long tone's
one tone, DTMF or KNOX digits, a Morse ID's text. Lengths are only what ▶ plays (pressed again, it
stops): they aren't matched, since pagers vary them. A signal is kept once it has what it needs.

## How near a tone has to be

A tone heard counts as a signal's within **±1.5 %** of it when it's measured in the audio (P25
Phase 1, analog): what selective-calling decoders are made to accept, and narrower than the 2.75 %
between neighbouring Quick Call II tones. A P25 Phase 2 tone frame names a tone only to the nearest
31.25 Hz, so from tone frames it's ±(15.7 Hz + 0.5 %); below about 1.1 kHz that's wider than the
step between QC2 tones, and neighbouring tones can't be told apart there. **Extra tolerance, %**
widens both. The note under a signal being edited says what that comes to for its tones, and names
any other paging plan's tone it would take in too (Quick Call I and II, GE Type 99, Reach, AvCall).
Digits and Morse text match exactly.

## Where signals go

- **The call's JSON**: `tone_patterns` lists every pattern with the signals it is, and `signals`
  lists the call's signals. See [Recordings](recordings.md#the-call-json).
- **The log**: each pattern heard, and the signals it is, as an info line.
- **The event feed** on the dashboard: each signal heard.
- **Plugins** subscribed to `tone_pattern` hear every pattern as soon as it's over, with the
  signals it is (if any). A plugin's [delays](../configuration/plugins.md#delays) hold it, as they
  do its calls; one heard in voice decrypted with your keys goes only to plugins allowed decrypted
  calls. See [Writing plugins](../plugins/writing-plugins.md#topics).

The signal settings are in the [systems reference](../configuration/systems.md#signals).

## What it costs

Finding tones costs about 0.013 % of one processor core for each call being recorded (measured on
ten minutes of speech with pages in it): next to nothing beside receiving and decoding.
