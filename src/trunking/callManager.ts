// Trunked call lifecycle — the protocol-agnostic core of Trunk Recorder's
// monitor_systems.cc (handle_call_grant / handle_call_update / manage_calls),
// minus the multi-site duplicate/superseding logic (one system per source here).
//
// A call is identified, as in Trunk Recorder, by (talkgroup, freq, TDMA slot).
// GRANTs (and UPDATEs when `newCallFromUpdate`) create calls; UPDATEs refresh
// them. A RECORDING call ends when BOTH the control channel has not mentioned it
// for `callTimeoutS` AND its recorder has written no audio for `callTimeoutS`;
// a MONITORING call ends on the first condition alone. Time is the sample clock
// (seconds of air since the source started), never wall time, so a replayed
// capture behaves exactly like the live air it came from.
//
// Recording itself (channels, decoders, audio) belongs to whoever owns the
// radio; the manager asks it through `RecorderHost`.

import type { TrunkMessage } from "../protocols/types.ts";
import { isEncryptedMode, type Talkgroup } from "./talkgroups.ts";

export type CallState = "recording" | "monitoring";
export type MonitorReason = "unknown_tg" | "encrypted" | "no_source" | "no_recorder" | null;

export interface CallSource {
  src: number;
  /** Sample-clock seconds this source was first heard on the call. */
  timeS: number;
  emergency: boolean;
}

export interface Call {
  id: number;
  talkgroup: number;
  freqHz: number;
  phase2Tdma: boolean;
  tdmaSlot: number;
  unitToUnit: boolean;
  state: CallState;
  reason: MonitorReason;
  encrypted: boolean;
  emergency: boolean;
  priority: number;
  duplex: boolean;
  mode: boolean;
  startS: number;
  lastUpdateS: number;
  /** Last time the recorder delivered audio (sample clock). */
  lastAudioS: number;
  sources: CallSource[];
  talkgroupInfo: Talkgroup | null;
}

export interface CallManagerConfig {
  callTimeoutS: number;
  recordUnknown: boolean;
  /** Record calls flagged encrypted (you get silence; Trunk Recorder's monitorEncrypted). */
  recordEncrypted: boolean;
  recordUnitToUnit: boolean;
  newCallFromUpdate: boolean;
}

export const DEFAULT_CALL_CONFIG: CallManagerConfig = {
  callTimeoutS: 3,
  recordUnknown: true,
  recordEncrypted: false,
  recordUnitToUnit: true,
  newCallFromUpdate: true,
};

export interface RecorderHost {
  /** Start recording `call`; false when the frequency is out of band or no recorder is free. */
  startRecording(call: Call): "ok" | "no_source" | "no_recorder";
  stopRecording(call: Call): void;
}

export interface CallEvents {
  onCallStart?(call: Call): void;
  onCallUpdate?(call: Call): void;
  /** The call is over. For a recording call this is when to finalise audio. */
  onCallEnd?(call: Call): void;
}

export class CallManager {
  readonly calls: Call[] = [];
  private nextId = 1;
  private readonly cfg: CallManagerConfig;
  private readonly host: RecorderHost;
  private readonly events: CallEvents;
  talkgroups = new Map<number, Talkgroup>();
  /** Active patches: supergroup → member talkgroups. */
  private readonly patches = new Map<number, { members: Set<number>; lastS: number }>();

  constructor(host: RecorderHost, events: CallEvents = {}, cfg: Partial<CallManagerConfig> = {}) {
    this.host = host;
    this.events = events;
    this.cfg = { ...DEFAULT_CALL_CONFIG, ...cfg };
  }

  handle(msgs: TrunkMessage[]): void {
    for (const m of msgs) {
      switch (m.type) {
        case "grant":
          this.grant(m, false);
          break;
        case "update":
          if (this.cfg.newCallFromUpdate) this.grant(m, false);
          else this.update(m);
          break;
        case "uu_v_grant":
          if (this.cfg.recordUnitToUnit) this.grant(m, true);
          break;
        case "uu_v_update":
          if (this.cfg.recordUnitToUnit) this.update(m);
          break;
        case "patch_add":
          if (m.patch) {
            const p = this.patches.get(m.patch.sg) ?? { members: new Set<number>(), lastS: m.timeS };
            for (const g of [m.patch.ga1, m.patch.ga2, m.patch.ga3]) if (g) p.members.add(g);
            p.lastS = m.timeS;
            this.patches.set(m.patch.sg, p);
          }
          break;
        case "patch_delete":
          if (m.patch) this.patches.delete(m.patch.sg);
          break;
        default:
          break;
      }
    }
  }

  /** Recorder delivered audio for this call. */
  noteAudio(call: Call, timeS: number): void {
    call.lastAudioS = Math.max(call.lastAudioS, timeS);
  }

  /** A voice channel named a source radio for this call. */
  noteSource(call: Call, src: number, timeS: number, emergency = false): void {
    if (src <= 0) return;
    const last = call.sources[call.sources.length - 1];
    if (last && last.src === src) return;
    call.sources.push({ src, timeS, emergency });
    this.events.onCallUpdate?.(call);
  }

  /** Advance the clock: end calls that have gone quiet. */
  tick(nowS: number): void {
    const t = this.cfg.callTimeoutS;
    for (let i = this.calls.length - 1; i >= 0; i--) {
      const c = this.calls[i];
      const quietCc = nowS - c.lastUpdateS > t;
      const quietAudio = nowS - c.lastAudioS > t;
      if (quietCc && (c.state === "monitoring" || quietAudio)) {
        this.calls.splice(i, 1);
        if (c.state === "recording") this.host.stopRecording(c);
        this.events.onCallEnd?.(c);
      }
    }
    for (const [sg, p] of this.patches) if (nowS - p.lastS > 60) this.patches.delete(sg);
  }

  /** End everything (source stopped). */
  endAll(): void {
    for (const c of this.calls.splice(0)) {
      if (c.state === "recording") this.host.stopRecording(c);
      this.events.onCallEnd?.(c);
    }
  }

  private matches(c: Call, m: TrunkMessage): boolean {
    return c.talkgroup === m.talkgroup && c.freqHz === m.freqHz && c.tdmaSlot === m.tdmaSlot && c.phase2Tdma === m.phase2Tdma;
  }

  private refresh(c: Call, m: TrunkMessage): void {
    c.lastUpdateS = m.timeS;
    if (m.encrypted) c.encrypted = true;
    if (m.emergency) c.emergency = true;
    if (m.source > 0) this.noteSource(c, m.source, m.timeS, m.emergency);
  }

  private update(m: TrunkMessage): void {
    for (const c of this.calls) if (this.matches(c, m)) this.refresh(c, m);
  }

  private grant(m: TrunkMessage, unitToUnit: boolean): void {
    if (!m.freqHz) return; // channel not yet resolvable (no IDEN seen)
    const existing = this.calls.find((c) => this.matches(c, m));
    if (existing) {
      this.refresh(existing, m);
      return;
    }
    const tg = this.talkgroups.get(m.talkgroup) ?? null;
    const call: Call = {
      id: this.nextId++,
      talkgroup: m.talkgroup,
      freqHz: m.freqHz,
      phase2Tdma: m.phase2Tdma,
      tdmaSlot: m.tdmaSlot,
      unitToUnit,
      state: "monitoring",
      reason: null,
      encrypted: m.encrypted || (tg ? isEncryptedMode(tg.mode) : false),
      emergency: m.emergency,
      priority: m.priority,
      duplex: m.duplex,
      mode: m.mode,
      startS: m.timeS,
      lastUpdateS: m.timeS,
      lastAudioS: m.timeS,
      sources: m.source > 0 ? [{ src: m.source, timeS: m.timeS, emergency: m.emergency }] : [],
      talkgroupInfo: tg,
    };
    // Trunk Recorder's start_recorder() gates, in its order.
    const patchedKnown = !tg && [...this.patches.entries()].some(([sg, p]) => (sg === m.talkgroup || p.members.has(m.talkgroup)) && [sg, ...p.members].some((g) => this.talkgroups.has(g)));
    if (!tg && !this.cfg.recordUnknown && !patchedKnown && this.talkgroups.size > 0) {
      call.reason = "unknown_tg";
    } else if (call.encrypted && !this.cfg.recordEncrypted) {
      call.reason = "encrypted";
    } else {
      const r = this.host.startRecording(call);
      if (r === "ok") call.state = "recording";
      else call.reason = r;
    }
    this.calls.push(call);
    this.events.onCallStart?.(call);
  }
}
