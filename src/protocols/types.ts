// The protocol seam. Everything above this file (call manager, recorders,
// storage, UI) is protocol-agnostic; everything below it is one radio system's
// physical layer and signalling. P25 is the only driver today; SmartNet, DMR and
// conventional systems plug in by implementing `ProtocolDriver`:
//
//   trunked (P25, SmartNet, DMR Tier III): a ControlDecoder turns the control
//     channel into TrunkMessages; the call manager allocates a channel per grant
//     and a VoiceDecoder per channel.
//   conventional (analog, P25, DMR): no ControlDecoder; each configured channel
//     gets a permanent VoiceDecoder, whose own activity (squelch / frame sync)
//     opens and closes calls.

/** Mirrors Trunk Recorder's `MessageType` (systems/parser.h). */
export type MessageType =
  | "grant"
  | "status"
  | "update"
  | "control_channel"
  | "registration"
  | "deregistration"
  | "affiliation"
  | "sysid"
  | "acknowledge"
  | "location"
  | "patch_add"
  | "patch_delete"
  | "data_grant"
  | "uu_ans_req"
  | "uu_v_grant"
  | "uu_v_update"
  | "invalid_cc_message"
  | "tdulc"
  | "call_alert"
  | "unknown";

export interface PatchData {
  sg: number;
  ga1: number;
  ga2: number;
  ga3: number;
}

/** Mirrors Trunk Recorder's `TrunkMessage`. Frequencies in Hz; 0 = unresolved. */
export interface TrunkMessage {
  type: MessageType;
  /** Seconds since the source started, on the sample clock (not wall time). */
  timeS: number;
  freqHz: number;
  talkgroup: number;
  /** Radio id; -1 when the message carries none. */
  source: number;
  encrypted: boolean;
  emergency: boolean;
  duplex: boolean;
  mode: boolean;
  priority: number;
  phase2Tdma: boolean;
  tdmaSlot: number;
  sysId: number;
  wacn: number;
  nac: number;
  rfss: number;
  site: number;
  patch: PatchData | null;
  opcode: number;
  /** Human-readable summary, as Trunk Recorder logs it. */
  meta: string;
}

export function blankMessage(timeS: number, opcode: number, nac = 0): TrunkMessage {
  return {
    type: "unknown",
    timeS,
    freqHz: 0,
    talkgroup: 0,
    source: -1,
    encrypted: false,
    emergency: false,
    duplex: false,
    mode: false,
    priority: 0,
    phase2Tdma: false,
    tdmaSlot: 0,
    sysId: 0,
    wacn: 0,
    nac,
    rfss: 0,
    site: 0,
    patch: null,
    opcode,
    meta: "",
  };
}

/** What a system has learned about itself from the air (or config). */
export interface SystemIdentity {
  nac: number | null;
  wacn: number | null;
  sysId: number | null;
  rfss: number | null;
  site: number | null;
}

export interface ControlStats {
  /** Messages that passed FEC/CRC. */
  good: number;
  /** Frames that failed FEC/CRC. */
  bad: number;
  /** Physical layer in use, e.g. "C4FM" / "CQPSK". */
  modulation: string | null;
}

export interface ControlDecoder {
  /** Interleaved IQ at the driver's channel rate, contiguous with the last push. */
  push(iq: Float32Array): void;
  stats(): ControlStats;
  identity(): SystemIdentity;
}

/** Per-transmission facts a voice decoder learns from the voice channel itself. */
export interface VoiceInfo {
  slot: number;
  talkgroup: number | null;
  source: number | null;
  encrypted: boolean;
  emergency: boolean;
}

export interface VoiceEvents {
  /** 8 kHz mono audio, [-1, 1], for one TDMA slot (0 for FDMA). */
  onAudio(slot: number, samples: Float32Array): void;
  /** A transmission started or its link control changed. */
  onInfo(info: VoiceInfo): void;
  /** A transmission ended (terminator seen, or the decoder lost it). */
  onTransmissionEnd(slot: number): void;
}

export interface VoiceDecoder {
  push(iq: Float32Array): void;
  /** Frames decoded / frames that failed FEC, for the call's error counts. */
  errorCounts(slot: number): { frames: number; errors: number };
  dispose(): void;
}

export interface ProtocolDriver {
  readonly type: string;
  /** Minimum channel output rate this driver's decoders want, samples/s. */
  readonly minChannelRate: number;
  /** One-sided channel filter cutoff, Hz. */
  readonly channelCutoffHz: number;
  readonly audioType: string;
  createControlDecoder?(rate: number, onMessages: (msgs: TrunkMessage[]) => void): ControlDecoder;
  createVoiceDecoder(rate: number, identity: SystemIdentity, events: VoiceEvents): VoiceDecoder;
}
