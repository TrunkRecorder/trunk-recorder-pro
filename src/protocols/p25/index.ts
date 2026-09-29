import type { ProtocolDriver } from "../types.ts";
import { P25ControlDecoder, type P25Modulation } from "./controlDecoder.ts";
import { P25VoiceDecoder } from "./voiceDecoder.ts";

/** P25 Phase 1 control channel; Phase 1 and Phase 2 voice. */
export function p25Driver(modulation: P25Modulation = "auto"): ProtocolDriver {
  return {
    type: "p25",
    // 4800 sym/s C4FM / CQPSK and 6000 sym/s H-DQPSK want ≥ ~5 samples/symbol.
    minChannelRate: 24_000,
    channelCutoffHz: 7_000,
    audioType: "digital",
    createControlDecoder: (rate, onMessages) => new P25ControlDecoder(rate, onMessages, modulation),
    createVoiceDecoder: (rate, identity, events) => new P25VoiceDecoder(rate, identity, events),
  };
}
