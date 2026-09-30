// P25 voice channel (Phase 1 FDMA or Phase 2 TDMA) → per-slot 8 kHz audio,
// wrapping freq-finder's live receiver. One instance per voice CHANNEL: on a
// Phase 2 carrier both slots come out of the same decoder, and the call manager
// routes each slot's audio to its own call.

import { P25LiveVoice, type P25Call, type TdmaKey } from "../../vendor/ff/p25Voice.ts";
import type { SystemIdentity, VoiceDecoder, VoiceEvents } from "../types.ts";

export class P25VoiceDecoder implements VoiceDecoder {
  private readonly live: P25LiveVoice;
  private readonly counts = [
    { frames: 0, errors: 0 },
    { frames: 0, errors: 0 },
  ];

  constructor(rate: number, identity: SystemIdentity, events: VoiceEvents) {
    const tdma: TdmaKey | null =
      identity.nac !== null && identity.sysId !== null && identity.wacn !== null
        ? { nac: identity.nac, sysid: identity.sysId, wacn: identity.wacn }
        : null;
    const info = (c: Readonly<Omit<P25Call, "audio">>) => ({
      slot: c.channel,
      talkgroup: c.tgid,
      source: c.source,
      encrypted: c.encrypted,
      emergency: c.emergency,
    });
    this.live = new P25LiveVoice(rate, {
      tdma,
      // A trunked voice channel is opened because a call is starting: retry
      // acquisition every window until it locks (freq-finder's default of 2 s
      // suits an idle channel someone is merely listening to).
      acquireIntervalS: 0.25,
      onAudio: (slot, samples) => events.onAudio(slot, samples),
      onCall: (c, ev) => {
        const k = this.counts[c.channel] ?? this.counts[0];
        k.frames = c.frames;
        k.errors = c.badFrames;
        if (ev === "end") events.onTransmissionEnd(c.channel);
        else events.onInfo(info(c));
      },
    });
  }

  push(iq: Float32Array): void {
    this.live.push(iq);
  }

  errorCounts(slot: number): { frames: number; errors: number } {
    return { ...(this.counts[slot] ?? this.counts[0]) };
  }

  dispose(): void {}
}
