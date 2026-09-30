// Messages between the main thread, the radio worker and the trunk worker.
//
//   main ──start/stop──▶ radio worker ◀──open/close (MessagePort)── trunk worker ◀──start/stop── main
//        ◀──stats/spectrum──              ── channel IQ via SampleRing (SAB) ──▶      ──status/calls/audio──▶ main

import type { ProConfig } from "../config.ts";
import type { SourceSettings } from "../sources/iqSource.ts";
import type { EngineStatus } from "../trunking/trunkEngine.ts";
import type { CallRecordJson } from "../recording/callRecord.ts";
import type { CallState, MonitorReason } from "../trunking/callManager.ts";

export type SourceSpec = { kind: "usb" } | { kind: "file"; file: File; realtime: boolean };

// ── main ⇄ radio ─────────────────────────────────────────────────────────────

export type ToRadio =
  | { type: "start"; source: SourceSpec; settings: SourceSettings; minOutputRate: number; historyS: number; trunkPort: MessagePort }
  | { type: "stop" };

export interface RadioStats {
  /** Wideband samples/s actually received over the last second. */
  rateMeasured: number;
  airS: number;
  /** DSP time ÷ air time, 0..1 (≈ share of one core). */
  load: number;
  heads: number;
  /** Channel samples dropped because a ring was full (the trunk worker fell behind). */
  ringDrops: number;
  /** Times the dongle was reopened after a stuck USB transfer. */
  usbRecoveries: number;
  spectrum: Float32Array;
  centerHz: number;
  rateHz: number;
}

export type FromRadio =
  | { type: "started"; outputRate: number }
  | { type: "stats"; stats: RadioStats }
  | { type: "ended" }
  | { type: "error"; message: string };

// ── trunk ⇄ radio (MessagePort) ──────────────────────────────────────────────

export type TrunkToRadio =
  | { type: "open"; id: number; offsetHz: number; cutoffHz: number; prerollS: number; ring: SharedArrayBuffer }
  | { type: "close"; id: number };

// ── main ⇄ trunk ─────────────────────────────────────────────────────────────

export type ToTrunk =
  | { type: "start"; config: ProConfig; channelRate: number; epochMsAtZero: number; radioPort: MessagePort }
  | { type: "stop" };

export interface CallView {
  id: number;
  talkgroup: number;
  alphaTag: string;
  freqHz: number;
  slot: number | null;
  state: CallState;
  reason: MonitorReason;
  encrypted: boolean;
  emergency: boolean;
  startS: number;
  sources: number[];
}

export interface LogLine {
  timeS: number;
  kind: string;
  text: string;
}

export interface CallEntry {
  baseName: string;
  dir: string;
  record: CallRecordJson;
}

export type FromTrunk =
  | { type: "status"; status: EngineStatus; calls: CallView[]; backlogS: number }
  | { type: "log"; lines: LogLine[] }
  | { type: "concluded"; entry: CallEntry }
  | { type: "audio"; callId: number; talkgroup: number; samples: Float32Array }
  | { type: "error"; message: string };
