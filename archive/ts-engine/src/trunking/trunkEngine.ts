// One trunked system, end to end, above the radio: the equivalent of Trunk
// Recorder's monitor_messages() loop plus its recorders.
//
//   control channel IQ → ControlDecoder → TrunkMessages → CallManager
//   CallManager.startRecording → a channel (head) per voice frequency, with
//     pre-roll, and a VoiceDecoder per channel; each TDMA slot's audio goes to
//     the call on that slot
//   CallManager.onCallEnd → a concluded call (JSON + audio) for storage
//
// The engine only talks to the radio through `ChannelPort`, so the same code
// runs inside the browser's trunk worker (port = message the radio worker,
// read SampleRings) and in node against a recorded capture (port = direct calls
// into a Channelizer). Time is the control channel's sample clock.

import type { ControlDecoder, ProtocolDriver, SystemIdentity, TrunkMessage, VoiceDecoder } from "../protocols/types.ts";
import { CallManager, type Call, type CallManagerConfig } from "./callManager.ts";
import type { Talkgroup } from "./talkgroups.ts";
import { callBaseName, callRecord, type CallRecordJson } from "../recording/callRecord.ts";

export interface ChannelPort {
  /** Start delivering channel IQ for `offsetHz` (from the tuned centre) to `onIq`. */
  open(offsetHz: number, cutoffHz: number, prerollS: number, onIq: (iq: Float32Array) => void): number;
  close(id: number): void;
}

export interface SystemConfig {
  shortName: string;
  type: string;
  controlChannels: number[];
  talkgroups?: Map<number, Talkgroup>;
}

export interface EngineConfig {
  system: SystemConfig;
  centerHz: number;
  rateHz: number;
  channelRate: number;
  /** Seconds of air a new voice channel replays from before its grant. */
  prerollS: number;
  maxRecorders: number;
  /** Keep calls with no decoded audio (encrypted, lost). Trunk Recorder keeps them. */
  keepSilentCalls: boolean;
  calls: Partial<CallManagerConfig>;
  /** Wall-clock epoch ms at sample-clock time 0. */
  epochMsAtZero: number;
}

export interface ConcludedCall {
  record: CallRecordJson;
  baseName: string;
  audio: Float32Array;
  audioRate: number;
}

export interface EngineEvents {
  onMessages?(msgs: TrunkMessage[]): void;
  onCallStart?(call: Call): void;
  onCallUpdate?(call: Call): void;
  onCallEnd?(call: Call): void;
  onConcluded?(c: ConcludedCall): void;
  onAudio?(call: Call, samples: Float32Array): void;
  onControlChannel?(freqHz: number): void;
}

interface Channel {
  freqHz: number;
  headId: number;
  decoder: VoiceDecoder;
  slots: [Call | null, Call | null];
}

interface Recording {
  chunks: Float32Array[];
  samples: number;
  recorderNum: number;
  channel: Channel;
}

export interface EngineStatus {
  nowS: number;
  controlChannelHz: number | null;
  identity: SystemIdentity;
  good: number;
  bad: number;
  modulation: string | null;
  activeCalls: number;
  recording: number;
  channelsOpen: number;
  callsConcluded: number;
}

/** Usable fraction of the source bandwidth (the anti-alias roll-off eats the edges). */
const USABLE = 0.9;
/** No good control message for this long → hunt to the next control channel. */
const CC_HUNT_S = 5;

export class TrunkEngine {
  readonly calls: CallManager;
  private readonly cfg: EngineConfig;
  private readonly driver: ProtocolDriver;
  private readonly port: ChannelPort;
  private readonly events: EngineEvents;
  private cc: ControlDecoder | null = null;
  private ccHead: number | null = null;
  private ccIndex = 0;
  private ccHz: number | null = null;
  private ccSamples = 0;
  private ccStartS = 0;
  private lastGoodS = 0;
  private lastGood = 0;
  private readonly channels = new Map<number, Channel>();
  private readonly recordings = new Map<number, Recording>();
  private readonly freeRecorderNums: number[] = [];
  private nextRecorderNum = 0;
  private concluded = 0;
  private nowS = 0;

  constructor(cfg: EngineConfig, driver: ProtocolDriver, port: ChannelPort, events: EngineEvents = {}) {
    this.cfg = cfg;
    this.driver = driver;
    this.port = port;
    this.events = events;
    this.calls = new CallManager(
      {
        startRecording: (c) => this.startRecording(c),
        stopRecording: (c) => this.stopRecording(c),
      },
      {
        onCallStart: (c) => events.onCallStart?.(c),
        onCallUpdate: (c) => events.onCallUpdate?.(c),
        onCallEnd: (c) => this.callEnded(c),
      },
      cfg.calls,
    );
    if (cfg.system.talkgroups) this.calls.talkgroups = cfg.system.talkgroups;
  }

  start(): void {
    this.tuneControl(0);
  }

  stop(): void {
    this.calls.endAll();
    if (this.ccHead !== null) this.port.close(this.ccHead);
    this.ccHead = null;
    for (const ch of this.channels.values()) this.port.close(ch.headId);
    this.channels.clear();
  }

  inBand(freqHz: number): boolean {
    return Math.abs(freqHz - this.cfg.centerHz) <= (this.cfg.rateHz / 2) * USABLE;
  }

  status(): EngineStatus {
    const s = this.cc?.stats();
    return {
      nowS: this.nowS,
      controlChannelHz: this.ccHz,
      identity: this.cc?.identity() ?? { nac: null, wacn: null, sysId: null, rfss: null, site: null },
      good: s?.good ?? 0,
      bad: s?.bad ?? 0,
      modulation: s?.modulation ?? null,
      activeCalls: this.calls.calls.length,
      recording: this.recordings.size,
      channelsOpen: this.channels.size,
      callsConcluded: this.concluded,
    };
  }

  /** Move to control channel `index` (mod the list), skipping any out of band. */
  private tuneControl(index: number): void {
    const list = this.cfg.system.controlChannels.filter((f) => this.inBand(f));
    if (!list.length) throw new Error("No control channel falls inside the source's bandwidth — move the center frequency.");
    this.ccIndex = ((index % list.length) + list.length) % list.length;
    const hz = list[this.ccIndex];
    if (this.ccHead !== null) this.port.close(this.ccHead);
    this.ccHz = hz;
    this.ccStartS = this.nowS;
    this.ccSamples = 0;
    this.lastGoodS = this.nowS;
    const cc = this.driver.createControlDecoder!(this.cfg.channelRate, (msgs) => this.onMessages(msgs));
    this.cc = cc;
    this.lastGood = 0;
    this.ccHead = this.port.open(hz - this.cfg.centerHz, this.driver.channelCutoffHz, 0, (iq) => this.onControlIq(iq));
    this.events.onControlChannel?.(hz);
  }

  private onControlIq(iq: Float32Array): void {
    this.cc?.push(iq);
    this.ccSamples += iq.length >> 1;
    this.nowS = this.ccStartS + this.ccSamples / this.cfg.channelRate;
    const good = this.cc?.stats().good ?? 0;
    if (good > this.lastGood) {
      this.lastGood = good;
      this.lastGoodS = this.nowS;
    } else if (this.nowS - this.lastGoodS > CC_HUNT_S && this.cfg.system.controlChannels.length > 1) {
      this.tuneControl(this.ccIndex + 1);
    }
    this.calls.tick(this.nowS);
  }

  private onMessages(msgs: TrunkMessage[]): void {
    for (const m of msgs) m.timeS += this.ccStartS;
    this.calls.handle(msgs);
    this.events.onMessages?.(msgs);
  }

  private startRecording(call: Call): "ok" | "no_source" | "no_recorder" {
    if (!this.inBand(call.freqHz)) return "no_source";
    if (this.recordings.size >= this.cfg.maxRecorders) return "no_recorder";
    let ch = this.channels.get(call.freqHz);
    if (!ch) ch = this.openChannel(call.freqHz);
    const slot = call.phase2Tdma ? call.tdmaSlot & 1 : 0;
    ch.slots[slot] = call; // a newer call on the same channel/slot takes it over, as in Trunk Recorder
    const recorderNum = this.freeRecorderNums.pop() ?? this.nextRecorderNum++;
    this.recordings.set(call.id, { chunks: [], samples: 0, recorderNum, channel: ch });
    return "ok";
  }

  private stopRecording(call: Call): void {
    const rec = this.recordings.get(call.id);
    if (!rec) return;
    const ch = rec.channel;
    for (let s = 0; s < 2; s++) if (ch.slots[s] === call) ch.slots[s] = null;
    if (!ch.slots[0] && !ch.slots[1]) {
      this.port.close(ch.headId);
      ch.decoder.dispose();
      this.channels.delete(ch.freqHz);
    }
  }

  private openChannel(freqHz: number): Channel {
    const identity = this.cc?.identity() ?? { nac: null, wacn: null, sysId: null, rfss: null, site: null };
    const ch: Channel = { freqHz, headId: -1, decoder: null as unknown as VoiceDecoder, slots: [null, null] };
    ch.decoder = this.driver.createVoiceDecoder(this.cfg.channelRate, identity, {
      onAudio: (slot, samples) => {
        const call = ch.slots[slot] ?? (ch.slots[0] && !ch.slots[0].phase2Tdma ? ch.slots[0] : null);
        if (!call) return;
        const rec = this.recordings.get(call.id);
        if (!rec) return;
        rec.chunks.push(samples.slice());
        rec.samples += samples.length;
        this.calls.noteAudio(call, this.nowS);
        this.events.onAudio?.(call, samples);
      },
      onInfo: (info) => {
        const call = ch.slots[info.slot] ?? ch.slots[0];
        if (!call) return;
        if (info.encrypted) call.encrypted = true;
        if (info.emergency) call.emergency = true;
        if (info.source) this.calls.noteSource(call, info.source, this.nowS, info.emergency);
      },
      onTransmissionEnd: () => {},
    });
    this.channels.set(freqHz, ch);
    ch.headId = this.port.open(freqHz - this.cfg.centerHz, this.driver.channelCutoffHz, this.cfg.prerollS, (iq) => ch.decoder.push(iq));
    return ch;
  }

  private callEnded(call: Call): void {
    this.events.onCallEnd?.(call);
    const rec = this.recordings.get(call.id);
    if (!rec) return;
    this.recordings.delete(call.id);
    this.freeRecorderNums.push(rec.recorderNum);
    // An encrypted call's "audio" is at most a few frames vocoded before the
    // decoder saw the ALGID (LDU1 precedes the LDU2 that carries it) — noise,
    // never speech. Trunk Recorder keeps no audio for these either.
    const samples = call.encrypted ? 0 : rec.samples;
    if (!samples && !this.cfg.keepSilentCalls) return;
    const audio = new Float32Array(samples);
    let o = 0;
    for (const c of samples ? rec.chunks : []) {
      audio.set(c, o);
      o += c.length;
    }
    const slot = call.phase2Tdma ? call.tdmaSlot & 1 : 0;
    const errs = rec.channel.decoder.errorCounts(slot);
    const record = callRecord(call, call.lastAudioS, {
      shortName: this.cfg.system.shortName,
      audioType: this.driver.audioType,
      epochMsAtZero: this.cfg.epochMsAtZero,
      audioSeconds: audio.length / 8000,
      errorCount: errs.errors,
      recorderNum: rec.recorderNum,
    });
    this.concluded++;
    this.events.onConcluded?.({ record, baseName: callBaseName(record), audio, audioRate: 8000 });
  }
}
