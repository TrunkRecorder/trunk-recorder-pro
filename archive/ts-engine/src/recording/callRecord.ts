// The per-call JSON, with Trunk Recorder's field names (call_concluder.cc
// create_call_json) so existing tooling can read it. Fields Lite can't measure
// yet (signal/noise, freq error, spike counts) are 0.

import type { Call } from "../trunking/callManager.ts";

export interface CallRecordJson {
  call_num: number;
  freq: number;
  freq_error: number;
  signal: number;
  noise: number;
  source_num: number;
  recorder_num: number;
  tdma_slot: number;
  phase2_tdma: number;
  start_time: number;
  stop_time: number;
  start_time_ms: number;
  stop_time_ms: number;
  emergency: number;
  priority: number;
  mode: number;
  duplex: number;
  encrypted: number;
  call_length: number;
  call_length_ms: number;
  talkgroup: number;
  talkgroup_tag: string;
  talkgroup_description: string;
  talkgroup_group_tag: string;
  talkgroup_group: string;
  color_code: number;
  audio_type: string;
  short_name: string;
  freqList: { freq: number; time: number; pos: number; len: number; error_count: number; spike_count: number }[];
  srcList: { src: number; time: number; pos: number; emergency: number; signal_system: string; tag: string; tag_ota: string }[];
}

export interface ConcludeInfo {
  shortName: string;
  audioType: string;
  /** Wall-clock epoch ms of sample-clock time 0. */
  epochMsAtZero: number;
  audioSeconds: number;
  errorCount: number;
  recorderNum: number;
}

export function callRecord(call: Call, endS: number, info: ConcludeInfo): CallRecordJson {
  const ms = (s: number) => Math.round(info.epochMsAtZero + s * 1000);
  const startMs = ms(call.startS);
  const stopMs = ms(endS);
  const tg = call.talkgroupInfo;
  return {
    call_num: call.id,
    freq: Math.round(call.freqHz),
    freq_error: 0,
    signal: 0,
    noise: 0,
    source_num: 0,
    recorder_num: info.recorderNum,
    tdma_slot: call.tdmaSlot,
    phase2_tdma: call.phase2Tdma ? 1 : 0,
    start_time: Math.floor(startMs / 1000),
    stop_time: Math.floor(stopMs / 1000),
    start_time_ms: startMs,
    stop_time_ms: stopMs,
    emergency: call.emergency ? 1 : 0,
    priority: call.priority,
    mode: call.mode ? 1 : 0,
    duplex: call.duplex ? 1 : 0,
    encrypted: call.encrypted ? 1 : 0,
    call_length: Math.round(info.audioSeconds),
    call_length_ms: Math.round(info.audioSeconds * 1000),
    talkgroup: call.talkgroup,
    talkgroup_tag: tg?.alphaTag || "",
    talkgroup_description: tg?.description || "",
    talkgroup_group_tag: tg?.tag || "",
    talkgroup_group: tg?.group || "",
    color_code: -1,
    audio_type: info.audioType + (call.phase2Tdma ? " tdma" : ""),
    short_name: info.shortName,
    freqList: [{ freq: Math.round(call.freqHz), time: Math.floor(startMs / 1000), pos: 0, len: Math.round(info.audioSeconds * 100) / 100, error_count: info.errorCount, spike_count: 0 }],
    srcList: call.sources.map((s) => ({
      src: s.src,
      time: Math.floor(ms(s.timeS) / 1000),
      pos: Math.max(0, Math.round((s.timeS - call.startS) * 100) / 100),
      emergency: s.emergency ? 1 : 0,
      signal_system: "",
      tag: "",
      tag_ota: "",
    })),
  };
}

/** Trunk Recorder's default file base name: <talkgroup>-<start epoch>_<freq>[.slot]. */
export function callBaseName(r: CallRecordJson): string {
  return `${r.talkgroup}-${r.start_time}_${r.freq}${r.phase2_tdma ? `.${r.tdma_slot}` : ""}`;
}
