// P25 voice decode — Phase 1 (FDMA, IMBE) and Phase 2 (TDMA, AMBE+2) — from IQ
// to 8 kHz audio, one record per call.
//
//   IQ → demod (C4FM / CQPSK at 4800, or H-DQPSK at 6000)
//      → windowed frame finder (P1 frame sync + NID, or P2 S-ISCH + slot framer)
//      → P25VoiceDecoder (call tracking: HDU/LDU1/LDU2/TDU, or MAC_PTT/4V/2V/ESS)
//      → FEC (dsp/p25/voice.ts, dsp/p25/phase2.ts)
//      → MBE vocoder (dsp/mbe/mbe.ts, mbelib port)
//
// The demod runs in overlapping windows (~0.5 s) because the C4FM receiver picks
// ONE sampling phase per block: over a multi-second call a 50 ppm clock offset
// between transmitter and dongle walks the symbol clock by whole symbols, while
// over half a second it walks ~0.03 of one. Each frame is claimed by exactly one
// window (by its start time), so nothing is decoded twice; the live receiver
// (P25LiveVoice) runs the same windows over a sliding buffer.
//
// **Encrypted voice is detected and reported, never "decoded".** The ALGID from
// the HDU / LDU2 / ESS / MAC_PTT (and the encrypted bit in the link control's
// service options) marks a call encrypted; its frames are counted and no audio
// is synthesised — there is no key handling here.
//
// Phase 2 needs the system's WACN, System ID and NAC to descramble (the burst
// XOR mask is seeded from them). They come from a control-channel harvest (the
// P25 system summary) or from the user; without them a Phase 2 capture is
// still identified — slots, burst types, calls — but carries no audio.
//
// Verified in web/test/p25voiceDecode.test.ts: encoder-built P1 and P2 calls,
// C4FM / CQPSK / H-DQPSK modulated to IQ with noise and a carrier offset,
// decode to the vocoder frames that were sent (the audio compared against
// running the vocoder on the transmitted parameters directly), plus the
// encrypted, wrong-key and no-key cases.

import { demodC4fmTracked, dibitsToBits, invertDibit, P25_SYMBOL_RATE, SYNC_DIBITS } from "./p25/c4fm.ts";
import { decodeNid } from "./p25/nid.ts";
import { measureSignalCenter } from "./p25/tuning.ts";
import { measureModulation } from "./p25/modulation.ts";
import { demodulate } from "./demod.ts";
import type { DecodedMessage } from "./workbench.ts";
import { MbeDecoder, mbeToUnit, MBE_FRAME_SAMPLES, MBE_SAMPLE_RATE, type MbeProfile, type MbeRng } from "./mbe/mbe.ts";
import {
  ALGID_CLEAR,
  algName,
  decodeHdu,
  decodeLdu1Lc,
  decodeLdu2Es,
  decodeTdulc,
  DUID_HDU,
  DUID_LDU1,
  DUID_LDU2,
  DUID_TDU,
  DUID_TDULC,
  imbeHeaderDecode,
  imbeParamsToBits,
  lduCodewordEndBit,
  lduCodewords,
  P1_FRAME_BITS,
} from "./p25/voice.ts";
import {
  BURST_2V,
  BURST_4V,
  BURST_FACCH_S,
  BURST_FACCH_U,
  BURST_LCCH_S,
  BURST_SACCH_S,
  BURST_SACCH_U,
  decodeAcch,
  decodeEss,
  decodeVcw,
  duidDecode,
  framePackets,
  ischLookup,
  P2_SLOT_DIBITS,
  P2_SYMBOL_RATE,
  p2XorMask,
  parseMacPtt,
  readEssA,
  readEssB,
  SLOT_CHANNEL,
  type P2Packet,
} from "./p25/phase2.ts";

export { algName, ALGID_CLEAR, MBE_SAMPLE_RATE };

/** "Encrypted, cipher not yet known": the LC's protect flag or encrypted
 *  service-option bit arrived before any ALGID did. */
export const ALGID_UNKNOWN = -1;

/** The three numbers that seed the Phase 2 scrambler. */
export interface TdmaKey {
  nac: number;
  sysid: number;
  wacn: number;
}

/** Which receiver: P1 C4FM, P1 simulcast CQPSK/LSM, or P2 H-DQPSK. */
export type P25VoiceDemod = "c4fm" | "cqpsk" | "hdqpsk";

export interface P25Call {
  id: number;
  phase: 1 | 2;
  /** Phase 2 logical channel (TDMA slot) 0/1; always 0 for Phase 1. */
  channel: number;
  nac: number | null;
  tgid: number | null;
  /** Unit-to-unit destination radio, when the LC says so. */
  target: number | null;
  source: number | null;
  emergency: boolean;
  algid: number;
  keyid: number;
  encrypted: boolean;
  /** Seconds into the capture. */
  startS: number;
  endS: number;
  /** 20 ms vocoder frames seen / synthesised as voice / repeated or muted (FEC). */
  frames: number;
  voiceFrames: number;
  badFrames: number;
  /** Bits corrected by the vocoder-frame FEC, summed. */
  fecErrors: number;
  /** Frames discarded because the receiver lost samples inside them (see
   *  `cutAtBit`) — played as a repeat of the last good frame. */
  erasedFrames: number;
  /** Phase 2 voice heard but not descrambled: no WACN / System ID / NAC given. */
  noKey: boolean;
  /** 8 kHz mono. Empty when encrypted. */
  audio: Float32Array;
}

export interface P25VoiceStats {
  demod: P25VoiceDemod | null;
  phase: 1 | 2 | null;
  inverted: boolean;
  /** P1: frames by type; P2: slot packets by burst type. */
  frames: Record<string, number>;
  /** P2 scrambled bursts that could not be read (no or wrong WACN/SysID/NAC). */
  undecodableBursts: number;
  /** P2: of the scrambled voice bursts, how many descrambled to a valid voice burst. */
  keyCheck: { tried: number; ok: number } | null;
  nac: number | null;
}

export interface P25VoiceResult {
  calls: P25Call[];
  stats: P25VoiceStats;
  info: string;
}

export interface P25VoiceOptions {
  /** Phase 2 scrambler seed. */
  tdma?: TdmaKey | null;
  /** Force a receiver; default: measured. */
  demod?: P25VoiceDemod | "auto";
  /** Vocoder RNG (tests replay a fixed one). */
  rand?: MbeRng;
  /** Vocoder behaviour: "enhanced" (default) or mbelib's exactly. */
  vocoder?: MbeProfile;
}

// ── Frame events (what the windowed front end hands the call tracker) ────────

export type P25FrameEvent =
  | {
      kind: "p1";
      timeS: number;
      nac: number;
      duid: number;
      fb: Uint8Array;
      /** The NEXT frame's sync began this many bits into this one: samples
       *  were lost somewhere before it (a receiver overrun), so this frame's
       *  tail is the next frame's head. */
      cutAtBit?: number;
    }
  | { kind: "p2"; timeS: number; packet: P2Packet };

/** FEC error count that makes the vocoder treat a frame as lost (mbelib repeats past 5). */
const ERASED = 99;

/** A call ends after this long without one of its frames. */
const CALL_GAP_S = 1.0;

interface ChannelState {
  dec: MbeDecoder;
  call: (Omit<P25Call, "audio"> & { chunks: Float32Array[] }) | null;
  algid: number;
  keyid: number;
  // Phase 2 (op25 p25p2_tdma)
  burstId: number;
  first4v: number;
  essB: number[];
  nextAlgid: number;
  nextKeyid: number;
}

export interface P25VoiceDecoderOptions {
  tdma?: TdmaKey | null;
  rand?: MbeRng;
  /** Vocoder behaviour: "enhanced" (default) or mbelib's exactly. */
  vocoder?: MbeProfile;
  /** Called with each 20 ms of synthesised audio (8 kHz, [-1, 1]). */
  onAudio?: (channel: number, samples: Float32Array, call: Readonly<Omit<P25Call, "audio">>) => void;
  /** Called when a call starts / updates / ends (metadata only). */
  onCall?: (call: Readonly<Omit<P25Call, "audio">>, event: "start" | "update" | "end") => void;
  /** Keep each call's audio in `calls` (default true; the live path turns it off). */
  keepAudio?: boolean;
}

/**
 * The call tracker: consumes frame events in time order, runs FEC and the
 * vocoder, and splits the stream into calls. Stateful — one instance per
 * channel being listened to. `onAudio` streams audio as it is synthesised (the
 * live receiver); the per-call record keeps it too (the workbench).
 */
export class P25VoiceDecoder {
  readonly calls: P25Call[] = [];
  readonly frames: Record<string, number> = {};
  undecodableBursts = 0;
  keyTried = 0;
  keyOk = 0;
  nac: number | null = null;
  private nextId = 1;
  private readonly ch: ChannelState[];
  private mask: Uint8Array | null;
  private readonly keepAudio: boolean;
  private readonly opts: P25VoiceDecoderOptions;

  constructor(opts: P25VoiceDecoderOptions = {}) {
    this.opts = opts;
    this.mask = opts.tdma ? p2XorMask(opts.tdma.nac, opts.tdma.sysid, opts.tdma.wacn) : null;
    this.keepAudio = opts.keepAudio ?? true;
    this.ch = [0, 1].map(() => ({
      dec: new MbeDecoder({ rand: opts.rand, profile: opts.vocoder }),
      call: null,
      algid: ALGID_CLEAR,
      keyid: 0,
      burstId: -1,
      first4v: -1,
      essB: new Array(16).fill(0),
      nextAlgid: ALGID_CLEAR,
      nextKeyid: 0,
    }));
  }

  setTdmaKey(key: TdmaKey | null): void {
    this.mask = key ? p2XorMask(key.nac, key.sysid, key.wacn) : null;
  }

  private count(k: string) {
    this.frames[k] = (this.frames[k] ?? 0) + 1;
  }

  private start(c: number, phase: 1 | 2, t: number, meta: Partial<P25Call> = {}): void {
    const s = this.ch[c];
    this.end(c, t);
    s.dec.reset();
    s.call = {
      id: this.nextId++,
      phase,
      channel: c,
      nac: this.nac,
      tgid: null,
      target: null,
      source: null,
      emergency: false,
      algid: s.algid,
      keyid: s.keyid,
      encrypted: s.algid !== ALGID_CLEAR,
      startS: t,
      endS: t,
      frames: 0,
      voiceFrames: 0,
      badFrames: 0,
      fecErrors: 0,
      erasedFrames: 0,
      noKey: false,
      chunks: [],
      ...meta,
    };
    this.opts.onCall?.(s.call, "start");
  }

  private end(c: number, t: number): void {
    const s = this.ch[c];
    // Phase 2 per-slot state starts over with the next call (Trunk Recorder
    // found a call that ended on a bad signal leaking into the next one on
    // the slot). A MAC_PTT re-establishes first4v right after this.
    s.burstId = -1;
    s.first4v = -1;
    s.essB.fill(0);
    s.nextAlgid = ALGID_CLEAR;
    s.nextKeyid = 0;
    if (!s.call) return;
    const { chunks, ...info } = s.call;
    s.call = null;
    if (info.frames === 0 && info.tgid === null) return; // a header with nothing after it
    let n = 0;
    for (const ch of chunks) n += ch.length;
    const audio = new Float32Array(info.encrypted ? 0 : n);
    if (!info.encrypted) {
      let o = 0;
      for (const ch of chunks) {
        audio.set(ch, o);
        o += ch.length;
      }
    }
    info.endS = Math.max(info.endS, Math.min(t, info.endS + CALL_GAP_S));
    this.calls.push({ ...info, audio });
    this.opts.onCall?.(info, "end");
  }

  /** Close every open call (end of capture). */
  finish(t: number): void {
    for (let c = 0; c < 2; c++) this.end(c, t);
    this.calls.sort((a, b) => a.startS - b.startS || a.channel - b.channel);
  }

  private setEncryption(c: number, algid: number, keyid: number): void {
    const s = this.ch[c];
    s.algid = algid;
    s.keyid = keyid;
    // Mark the call encrypted — or, when it was marked on the LC's encrypted
    // bit before any ALGID arrived, name the cipher now that one has.
    if (s.call && algid !== ALGID_CLEAR && (!s.call.encrypted || (s.call.algid === ALGID_UNKNOWN && algid !== ALGID_UNKNOWN))) {
      s.call.encrypted = true;
      s.call.algid = algid;
      s.call.keyid = keyid;
      this.opts.onCall?.(s.call, "update");
    }
  }

  /** Continue (or open) a call on channel c at time t. */
  private touch(c: number, phase: 1 | 2, t: number): void {
    const s = this.ch[c];
    if (s.call && t - s.call.endS > CALL_GAP_S) {
      // Went quiet with no terminator heard: the next transmission is a new
      // call and must re-learn its cipher.
      this.end(c, t);
      s.algid = ALGID_CLEAR;
      s.keyid = 0;
    }
    if (!s.call) this.start(c, phase, t);
    s.call!.endS = t;
  }

  /** `e0` = errors corrected in the first (pitch-carrying) Golay word — the
   *  IMBE concealment keys on it (TIA §7.7); AMBE uses the total only. */
  private voiceFrame(c: number, kind: "imbe" | "ambe", bits: Uint8Array, errs: number, e0 = 0): void {
    const s = this.ch[c];
    const call = s.call!;
    call.frames++;
    const erased = errs === ERASED;
    if (erased) call.erasedFrames++;
    else call.fecErrors += errs;
    if (call.encrypted) return;
    const out = new Float32Array(MBE_FRAME_SAMPLES);
    const k = kind === "imbe" ? s.dec.imbe(bits, { e0, et: erased ? 0 : errs, erased }, out) : s.dec.ambe(bits, errs, out);
    if (k === "voice" || k === "silence" || k === "tone" || k === "erasure") call.voiceFrames++;
    else call.badFrames++;
    mbeToUnit(out);
    if (this.keepAudio) call.chunks.push(out);
    this.opts.onAudio?.(c, out, call);
  }

  // ── Phase 1 (op25 p25p1_fdma process_frame) ────────────────────────────────

  p1(ev: { timeS: number; nac: number; duid: number; fb: Uint8Array; cutAtBit?: number }): void {
    const t = ev.timeS;
    this.nac = ev.nac;
    const s = this.ch[0];
    switch (ev.duid) {
      case DUID_HDU: {
        this.count("HDU");
        const h = decodeHdu(ev.fb);
        s.algid = h?.algid ?? ALGID_CLEAR;
        s.keyid = h?.keyid ?? 0;
        this.start(0, 1, t, { tgid: h?.tgid ?? null });
        return;
      }
      case DUID_LDU1:
      case DUID_LDU2: {
        this.count(ev.duid === DUID_LDU1 ? "LDU1" : "LDU2");
        this.touch(0, 1, t);
        const call = s.call!;
        if (ev.duid === DUID_LDU1) {
          const lc = decodeLdu1Lc(ev.fb);
          if (lc?.protected) this.setEncryption(0, s.algid === ALGID_CLEAR ? ALGID_UNKNOWN : s.algid, s.keyid);
          else if (lc && (lc.lco === 0 || lc.lco === 3)) {
            // A different talkgroup mid-stream (no terminator heard) is a new call.
            if (call.tgid !== null && lc.tgid !== null && call.tgid !== lc.tgid && call.frames > 0) this.start(0, 1, t);
            const cur = s.call!;
            const changed = cur.tgid !== lc.tgid || cur.source !== lc.source;
            cur.tgid = lc.tgid ?? cur.tgid;
            cur.target = lc.target ?? cur.target;
            cur.source = lc.source ?? cur.source;
            cur.emergency = !!(lc.svcOpts !== null && lc.svcOpts & 0x80);
            // Service options bit 6 = encrypted: known before the first LDU2.
            if (lc.svcOpts !== null && lc.svcOpts & 0x40 && s.algid === ALGID_CLEAR) this.setEncryption(0, ALGID_UNKNOWN, 0);
            if (changed) this.opts.onCall?.(cur, "update");
          }
        } else {
          // No key: an encrypted ALGID applies to THIS LDU2's voice too (op25
          // crypt_behavior ≥ 2), so its frames are never synthesised as noise.
          const es = decodeLdu2Es(ev.fb);
          if (es) this.setEncryption(0, es.algid, es.keyid);
        }
        // A cut frame (samples lost inside it): every codeword past the cut is
        // the next frame's data, and the gap sits somewhere before the cut, so
        // from the first codeword that fails its FEC onward nothing is trusted.
        // Those become erasures — the vocoder repeats the last good frame
        // (then mutes) instead of synthesising garbage as a click.
        let erased = false;
        lduCodewords(ev.fb).forEach((cw, f) => {
          const p = imbeHeaderDecode(cw);
          if (ev.cutAtBit !== undefined && (p.errs > 2 || lduCodewordEndBit(f) >= ev.cutAtBit)) erased = true;
          this.voiceFrame(0, "imbe", imbeParamsToBits(p.u), erased ? ERASED : p.errs, p.e0);
        });
        return;
      }
      case DUID_TDU:
      case DUID_TDULC: {
        this.count(ev.duid === DUID_TDU ? "TDU" : "TDULC");
        if (ev.duid === DUID_TDULC && s.call) {
          const lc = decodeTdulc(ev.fb);
          if (lc && !lc.protected && lc.tgid !== null && s.call.tgid === null) s.call.tgid = lc.tgid;
        }
        if (s.call) s.call.endS = t;
        this.end(0, t);
        s.algid = ALGID_CLEAR;
        s.keyid = 0;
        return;
      }
      default:
        this.count(ev.duid === 0x7 ? "TSDU (control)" : ev.duid === 0xc ? "PDU (data)" : `DUID ${ev.duid.toString(16)}`);
    }
  }

  // ── Phase 2 (op25 p25p2_tdma handle_packet) ────────────────────────────────

  p2(ev: { timeS: number; packet: P2Packet }): void {
    const t = ev.timeS;
    const { slot, dibits } = ev.packet;
    const c = SLOT_CHANNEL[slot];
    const s = this.ch[c];
    const burstp = dibits.subarray(10);
    const type = duidDecode(burstp);
    const scrambled = type === BURST_4V || type === BURST_2V || type === BURST_SACCH_S || type === BURST_LCCH_S || type === BURST_FACCH_S;
    const name =
      ({ [BURST_4V]: "4V", [BURST_2V]: "2V", [BURST_SACCH_S]: "SACCH", [BURST_FACCH_S]: "FACCH", [BURST_SACCH_U]: "SACCH (clear)", [BURST_FACCH_U]: "FACCH (clear)" } as Record<number, string>)[type] ??
      (type < 0 ? "bad DUID" : `burst ${type}`);
    this.count(name);
    if (type < 0) return;
    let x: Uint8Array;
    if (scrambled) {
      if (!this.mask) {
        this.undecodableBursts++;
        if (type === BURST_4V || type === BURST_2V) {
          // A call is happening; without the scrambler seed it cannot be heard.
          this.touch(c, 2, t);
          s.call!.noKey = true;
          s.call!.frames += type === BURST_4V ? 4 : 2;
        }
        return;
      }
      x = new Uint8Array(170);
      const m = this.mask;
      for (let i = 0; i < 170; i++) x[i] = burstp[i] ^ m[slot * P2_SLOT_DIBITS + i];
    } else {
      x = burstp.slice(0, 170);
    }

    if (type === BURST_4V || type === BURST_2V) {
      const currentSlot0 = slot >> 1;
      // op25 track_vb
      s.burstId++;
      s.burstId = type === BURST_4V ? s.burstId % 5 : 4;
      const lastRc = ischLookup(dibits, 0);
      if (type === BURST_2V && lastRc >= 0) s.first4v = (currentSlot0 + 1) % 5;
      if (s.first4v >= 0 && lastRc >= 0) {
        let cs = currentSlot0;
        if (cs < s.first4v) cs += 5;
        cs -= s.first4v;
        if (cs !== s.burstId && cs > s.burstId) s.burstId = cs;
      }
      // Key check: a 4V descrambled with the wrong key has a random-looking
      // FEC load; count how many decode with ≤ 2 bit errors across c0/c1.
      const f0 = decodeVcw(x, 11);
      this.keyTried++;
      if (f0.errs <= 2) this.keyOk++;

      this.touch(c, 2, t);
      // ESS (op25 handle_4V2V_ess)
      if (s.burstId < 4) {
        const hb = readEssB(x);
        for (let i = 0; i < 4; i++) s.essB[4 * s.burstId + i] = hb[i];
      } else {
        const ess = decodeEss(s.essB, readEssA(x));
        if (ess) {
          s.nextAlgid = ess.algid;
          s.nextKeyid = ess.keyid;
          if (ess.algid !== ALGID_CLEAR) this.setEncryption(c, ess.algid, ess.keyid);
        }
      }
      const at = type === BURST_4V ? [11, 48, 96, 133] : [11, 48];
      for (const a of at) {
        const f = a === 11 ? f0 : decodeVcw(x, a);
        this.voiceFrame(c, "ambe", f.bits, f.errs);
      }
      if (type === BURST_2V) this.setEncryption(c, s.nextAlgid, s.nextKeyid);
      return;
    }

    if (type === BURST_SACCH_S || type === BURST_SACCH_U || type === BURST_FACCH_S || type === BURST_FACCH_U) {
      const pdu = decodeAcch(x, type === BURST_FACCH_S || type === BURST_FACCH_U);
      if (!pdu) return;
      if (pdu.opcode === 1) {
        // MAC_PTT: a call starts — who, which talkgroup, which cipher.
        this.count("MAC_PTT");
        const ptt = parseMacPtt(pdu.bytes);
        s.algid = ptt.algid;
        s.keyid = ptt.keyid;
        const cur = s.call;
        // A PTT for a different talkgroup is a new call; a PTT arriving after
        // voice we joined mid-call (talkgroup still unknown) just names it.
        if (!cur || (cur.tgid !== null && cur.tgid !== ptt.group) || t - cur.endS > CALL_GAP_S)
          this.start(c, 2, t, { tgid: ptt.group, source: ptt.source });
        else {
          cur.tgid = ptt.group;
          cur.source = ptt.source;
          cur.endS = t;
          this.setEncryption(c, ptt.algid, ptt.keyid);
          this.opts.onCall?.(cur, "update");
        }
        s.first4v = ((slot >> 1) + pdu.offset + 1) % 5;
        s.burstId = -1;
      } else if (pdu.opcode === 2) {
        this.count("MAC_END_PTT");
        if (s.call) s.call.endS = t;
        this.end(c, t);
        s.algid = ALGID_CLEAR;
        s.keyid = 0;
      } else if (pdu.opcode === 4) {
        this.count("MAC_ACTIVE");
        s.first4v = pdu.offset > 4 ? 0 : pdu.offset;
      } else if (pdu.opcode === 3) {
        this.count("MAC_IDLE");
      }
    }
  }

  ingest(ev: P25FrameEvent): void {
    if (ev.kind === "p1") this.p1(ev);
    else this.p2(ev);
  }
}

/** One row per call for the generic decoded-message views (and pipelines). */
export function p25CallMessages(calls: P25Call[]): DecodedMessage[] {
  return calls.map((c) => {
    const who = c.tgid !== null ? `TG ${c.tgid}` : c.target !== null ? `unit ${c.target}` : "TG ?";
    const dur = Math.max(0, c.endS - c.startS);
    const crypt = c.noKey ? "scrambled (no key)" : c.encrypted ? algName(c.algid) : "clear";
    return {
      protocol: `P25 P${c.phase} voice`,
      address: who,
      label: crypt,
      text: `${c.source !== null ? `from ${c.source}` : "source ?"} · ${dur.toFixed(1)} s` + (c.phase === 2 ? ` · slot ${c.channel}` : "") + (c.emergency ? " · EMERGENCY" : ""),
      fields: [
        { label: "Talkgroup", value: c.tgid !== null ? String(c.tgid) : "" },
        { label: "Source", value: c.source !== null ? String(c.source) : "" },
        { label: "Cipher", value: crypt },
        { label: "Start (s)", value: c.startS.toFixed(2) },
        { label: "Duration (s)", value: dur.toFixed(2) },
        { label: "Frames", value: String(c.frames) },
        ...(c.nac !== null ? [{ label: "NAC", value: `0x${c.nac.toString(16)}` }] : []),
      ],
    };
  });
}

// ── The windowed front end ───────────────────────────────────────────────────

const SYNC_LEN = 24;
const MIN_SYNC_MATCH = 20;

function p1SyncAt(d: ArrayLike<number>, p: number): boolean {
  let m = 0;
  for (let i = 0; i < SYNC_LEN; i++) if (d[p + i] === SYNC_DIBITS[i]) m++;
  return m >= MIN_SYNC_MATCH;
}
function inverted(d: Uint8Array): Uint8Array {
  const o = new Uint8Array(d.length);
  for (let i = 0; i < d.length; i++) o[i] = invertDibit(d[i]);
  return o;
}

interface Receiver {
  demod: P25VoiceDemod;
  inverted: boolean;
  offsetHz: number;
}

function runDemod(
  rx: { demod: P25VoiceDemod; offsetHz: number },
  iq: Float32Array,
  fs: number,
): { dibits: Uint8Array; sps: number; instants: Float32Array | null } {
  if (rx.demod === "c4fm") {
    const r = demodC4fmTracked(iq, fs);
    return { dibits: r.dibits, sps: fs / P25_SYMBOL_RATE, instants: r.instants };
  }
  const baud = rx.demod === "hdqpsk" ? P2_SYMBOL_RATE : P25_SYMBOL_RATE;
  const r = demodulate(iq, fs, "pi4dqpsk", baud, { carrierOffsetHz: rx.offsetHz });
  return { dibits: r.symbols, sps: fs / baud, instants: r.sampleInstants.length === r.symbols.length ? r.sampleInstants : null };
}

/** Frames in a dibit stream that are REALLY P25: P1 syncs whose NID decodes,
 *  P2 slot packets whose DUID does (÷6: a slot is 30 ms, an LDU 180 ms). A
 *  wrong receiver still stumbles on syncs — CQPSK half-demodulates C4FM — but
 *  its NIDs fail, so this is the figure of merit, not the raw sync count. */
function validFrames(d: Uint8Array, phase2: boolean): number {
  if (phase2) {
    let n = 0;
    for (const p of framePackets(d)) if (duidDecode(p.dibits.subarray(10)) >= 0) n++;
    return n / 6;
  }
  let n = 0;
  for (let p = 0; p + SYNC_LEN + 33 <= d.length; p++) {
    if (!p1SyncAt(d, p)) continue;
    const hdr = dibitsToBits(d, p, 57);
    let acc = 0n;
    for (let i = 48; i < 114; i++) if (i !== 70 && i !== 71) acc = (acc << 1n) | BigInt(hdr[i]);
    const nid = decodeNid(acc);
    if (nid) {
      n++;
      // Voice frames also say how clean their payload is: a half-working
      // receiver (C4FM on LSM) passes the BCH-protected NID but not the IMBE
      // Golay words, so they break the tie.
      if ((nid.duid === DUID_LDU1 || nid.duid === DUID_LDU2) && p + 864 <= d.length) {
        let clean = 0;
        for (const cw of lduCodewords(dibitsToBits(d, p, 864))) if (imbeHeaderDecode(cw).errs <= 2) clean++;
        n += clean / 9;
      }
    }
    p += SYNC_LEN - 1;
  }
  return n;
}

/**
 * Pick the receiver: whichever of C4FM / CQPSK@4800 / H-DQPSK@6000, in either
 * polarity, yields the most frames that decode in the first ~1.5 s. All six
 * are tried — an early exit on "enough syncs" let CQPSK win a C4FM channel in
 * the browser (half-demodulated: syncs yes, voice no).
 */
export function detectVoiceReceiver(iq: Float32Array, fs: number, force: P25VoiceDemod | "auto" = "auto"): (Receiver & { syncs: number }) | null {
  const center = measureSignalCenter(iq, fs);
  const offsetHz = center?.offsetHz ?? 0;
  const head = iq.subarray(0, Math.min(iq.length, Math.round(fs * 1.5) * 2));
  const cands: P25VoiceDemod[] = force !== "auto" ? [force] : ["c4fm", "cqpsk", "hdqpsk"];
  const scored: (Receiver & { syncs: number })[] = [];
  for (const demod of cands) {
    if (fs / (demod === "hdqpsk" ? P2_SYMBOL_RATE : P25_SYMBOL_RATE) < 2) continue;
    const { dibits } = runDemod({ demod, offsetHz }, head, fs);
    for (const inv of [false, true]) {
      const syncs = validFrames(inv ? inverted(dibits) : dibits, demod === "hdqpsk");
      if (syncs >= 1) scored.push({ demod, inverted: inv, offsetHz, syncs });
    }
  }
  scored.sort((a, b) => b.syncs - a.syncs);
  const best = scored[0] ?? null;
  // Phase 1 either way, and both receivers decode it (π/4 phase steps ARE
  // C4FM's levels integrated over a symbol): when they tie, name the waveform
  // the envelope says it is — constant envelope is C4FM, a swinging one CQPSK.
  if (best && best.demod !== "hdqpsk") {
    const other = scored.find((r) => r.demod !== best.demod && r.demod !== "hdqpsk");
    if (other && other.syncs >= 0.95 * best.syncs) {
      const kind = measureModulation(head, fs)?.kind;
      const want = kind === "fsk4" ? "c4fm" : kind === "qpsk" ? "cqpsk" : null;
      if (want === other.demod) return other;
    }
  }
  return best;
}

const WINDOW_S = 0.5;
const LEAD_S = 0.2; // P2 framer needs an S-ISCH + I-ISCH before it trusts a slot number
const TAIL_S = 0.2; // ≥ one LDU (180 ms) so a frame starting in the window ends in it

/** Carries the last claimed frame start across windows (and live pushes). */
export interface FrameCursor {
  /** Absolute time (s) of the last frame handed on; -Infinity to start. */
  lastS: number;
}

/**
 * Frame events for samples [fromSample, toSample) of `iq` (interleaved, sample
 * units). The demod sees LEAD_S before and TAIL_S after each window. A frame
 * belongs to the window its start falls in — widened by a guard on the early
 * side and de-duplicated against `cursor`, because two windows' symbol clocks
 * can disagree about a boundary frame's start by a symbol or two. `baseS`
 * offsets the reported times (the live receiver's absolute clock).
 */
export function findFrames(
  rx: Receiver,
  iq: Float32Array,
  fs: number,
  fromSample: number,
  toSample: number,
  baseS = 0,
  cursor: FrameCursor = { lastS: -Infinity },
): P25FrameEvent[] {
  const out: P25FrameEvent[] = [];
  const nTotal = iq.length >> 1;
  const winN = Math.round(WINDOW_S * fs);
  const leadN = Math.round(LEAD_S * fs);
  const tailN = Math.round(TAIL_S * fs);
  const phase2 = rx.demod === "hdqpsk";
  // Half the shortest frame spacing: a P1 TDU is 72 symbols, a P2 slot 180.
  const guardS = phase2 ? 90 / P2_SYMBOL_RATE : 36 / P25_SYMBOL_RATE;
  const claim = (at: number, w0: number, w1: number): boolean => {
    const tS = baseS + at / fs;
    if (at < w0 - guardS * fs || at >= w1 || tS < cursor.lastS + guardS) return false;
    cursor.lastS = tS;
    return true;
  };
  for (let w0 = fromSample; w0 < toSample; w0 += winN) {
    const w1 = Math.min(w0 + winN, toSample);
    const a = Math.max(0, w0 - leadN);
    const b = Math.min(nTotal, w1 + tailN);
    const seg = iq.subarray(2 * a, 2 * b);
    const r = runDemod(rx, seg, fs);
    const d = rx.inverted ? inverted(r.dibits) : r.dibits;
    const sampleOf = (k: number) => a + (r.instants ? r.instants[k] : k * r.sps);
    if (!phase2) {
      for (let p = 0; p + SYNC_LEN + 33 <= d.length; p++) {
        if (!p1SyncAt(d, p)) continue;
        const at = sampleOf(p);
        if (at < w0 - guardS * fs || at >= w1) {
          p += SYNC_LEN - 1;
          continue;
        }
        // NID: the 64 bits after the sync, skipping the status dibit at 35.
        const hdr = dibitsToBits(d, p, 57);
        let acc = 0n;
        for (let i = 48; i < 114; i++) if (i !== 70 && i !== 71) acc = (acc << 1n) | BigInt(hdr[i]);
        const nid = decodeNid(acc);
        if (!nid) {
          p += SYNC_LEN - 1;
          continue;
        }
        const bits = P1_FRAME_BITS[nid.duid];
        if (bits === undefined) {
          // TSDU (trunking control) / PDU (data): not voice, but worth
          // reporting — the user may have pointed the voice decoder at a
          // control or data channel. Counted, not decoded.
          if (claim(at, w0, w1)) out.push({ kind: "p1", timeS: baseS + at / fs, nac: nid.nac, duid: nid.duid, fb: new Uint8Array(0) });
          p += SYNC_LEN - 1;
          continue;
        }
        // A frame may be short by up to 2 trailing dibits where the capture (or
        // the demod's last symbol) ends — an LDU's last dibit is a status
        // symbol; the rest pads with zeros and costs at most FEC margin.
        if (bits === undefined || p + bits / 2 - 2 > d.length) {
          p += SYNC_LEN - 1;
          continue;
        }
        // A sync INSIDE this frame means samples were lost: the next frame
        // started before this one had its full length.
        let cutAtBit: number | undefined;
        for (let q = p + SYNC_LEN + 40; q + SYNC_LEN <= Math.min(d.length, p + bits / 2 - 8); q++) {
          if (p1SyncAt(d, q)) {
            cutAtBit = 2 * (q - p);
            break;
          }
        }
        if (claim(at, w0, w1)) {
          const fb = new Uint8Array(bits);
          fb.set(dibitsToBits(d, p, Math.min(bits / 2, d.length - p)));
          out.push({ kind: "p1", timeS: baseS + at / fs, nac: nid.nac, duid: nid.duid, fb, ...(cutAtBit !== undefined ? { cutAtBit } : {}) });
        }
        // Resume at the next frame's sync — inside this one if it was cut.
        p += (cutAtBit !== undefined ? cutAtBit / 2 : bits / 2) - 1;
      }
    } else {
      for (const pkt of framePackets(d)) {
        if (claim(sampleOf(pkt.at), w0, w1)) out.push({ kind: "p2", timeS: baseS + sampleOf(pkt.at) / fs, packet: pkt });
      }
    }
  }
  return out;
}

/** One-shot decode of a capture (the workbench path). */
export function decodeP25Voice(iq: Float32Array, fs: number, opts: P25VoiceOptions = {}): P25VoiceResult {
  const rx = detectVoiceReceiver(iq, fs, opts.demod ?? "auto");
  const dec = new P25VoiceDecoder({ tdma: opts.tdma ?? null, rand: opts.rand, vocoder: opts.vocoder });
  const stats: P25VoiceStats = {
    demod: rx?.demod ?? null,
    phase: rx ? (rx.demod === "hdqpsk" ? 2 : 1) : null,
    inverted: rx?.inverted ?? false,
    frames: dec.frames,
    undecodableBursts: 0,
    keyCheck: null,
    nac: null,
  };
  if (!rx) return { calls: [], stats, info: "P25 voice: no P25 frame sync found (tried C4FM, CQPSK and Phase 2 H-DQPSK)" };
  const nSamples = iq.length >> 1;
  for (const ev of findFrames(rx, iq, fs, 0, nSamples)) dec.ingest(ev);
  dec.finish(nSamples / fs);
  stats.undecodableBursts = dec.undecodableBursts;
  stats.keyCheck = stats.phase === 2 && dec.keyTried ? { tried: dec.keyTried, ok: dec.keyOk } : null;
  stats.nac = dec.nac;
  return { calls: dec.calls, stats, info: summarise(dec.calls, stats, !!opts.tdma) };
}

function summarise(calls: P25Call[], s: P25VoiceStats, haveKey: boolean): string {
  const rxName = s.demod === "c4fm" ? "Phase 1 C4FM" : s.demod === "cqpsk" ? "Phase 1 CQPSK" : "Phase 2 TDMA";
  const nFrames = Object.values(s.frames).reduce((a, b) => a + b, 0);
  const parts = [`P25 voice: ${rxName}, ${nFrames} frame${nFrames === 1 ? "" : "s"}`];
  if (s.nac !== null) parts.push(`NAC 0x${s.nac.toString(16)}`);
  const clear = calls.filter((c) => !c.encrypted).length;
  const enc = calls.length - clear;
  parts.push(`${calls.length} call${calls.length === 1 ? "" : "s"}` + (enc ? ` (${enc} encrypted)` : ""));
  if (!calls.length && s.frames["TSDU (control)"])
    parts.push("this is a trunking control channel — run the P25 control channel decoder for the system, then tune a voice channel it grants");
  else if (!calls.length && s.frames["PDU (data)"]) parts.push("data packets only, no voice");
  if (s.phase === 2 && !haveKey && s.undecodableBursts)
    parts.push(`${s.undecodableBursts} scrambled bursts need the WACN / System ID / NAC to descramble`);
  if (s.keyCheck && s.keyCheck.tried >= 8 && s.keyCheck.ok / s.keyCheck.tried < 0.3)
    parts.push("the WACN / System ID / NAC do not descramble this channel — wrong system?");
  return parts.join(" · ");
}

// ── Live receiver (the Capture tab) ──────────────────────────────────────────

/**
 * Streams channel IQ (already mixed to DC and decimated) into the voice
 * decoder: keeps a sliding buffer, and every WINDOW_S of new samples decodes
 * the window that has aged past the tail margin. Audio comes out through
 * `onAudio` at 8 kHz, ~0.5–0.9 s behind the air (window + tail + one LDU).
 * The receiver (C4FM / CQPSK / H-DQPSK, polarity) is re-detected whenever
 * nothing has decoded for a few seconds, so tuning from a P1 to a P2 channel
 * just works.
 */
export class P25LiveVoice {
  private buf: Float32Array;
  private len = 0; // samples in buf
  private bufStart = 0; // absolute sample index of buf[0]
  private next = 0; // absolute sample index of the next window to decode
  private rx: Receiver | null = null;
  private lastFrameAt = 0;
  private lastAcquireAt = -Infinity;
  private readonly cursor: FrameCursor = { lastS: -Infinity };
  readonly decoder: P25VoiceDecoder;
  readonly fs: number;

  constructor(fs: number, opts: P25VoiceDecoderOptions & { demod?: P25VoiceDemod | "auto"; acquireIntervalS?: number } = {}) {
    this.fs = fs;
    this.acquireIntervalS = opts.acquireIntervalS ?? 2;
    this.decoder = new P25VoiceDecoder({ ...opts, keepAudio: false });
    this.force = opts.demod ?? "auto";
    this.buf = new Float32Array(2 * Math.round(fs * (LEAD_S + WINDOW_S * 3 + TAIL_S + 2)));
  }
  private readonly force: P25VoiceDemod | "auto";
  /** Trunk Recorder Pro: how often to retry acquisition while unlocked. */
  private readonly acquireIntervalS: number;

  get receiver(): P25VoiceDemod | null {
    return this.rx?.demod ?? null;
  }

  /** Feed interleaved IQ at `fs`. Decoding happens synchronously in here. */
  push(iq: Float32Array): void {
    const n = iq.length >> 1;
    if (2 * (this.len + n) > this.buf.length) this.compact(n);
    this.buf.set(iq, 2 * this.len);
    this.len += n;
    const winN = Math.round(WINDOW_S * this.fs);
    const tailN = Math.round(TAIL_S * this.fs);
    while (this.bufStart + this.len - this.next >= winN + tailN) {
      this.decodeWindow(winN);
    }
  }

  private compact(incoming: number): void {
    // Keep the lead-in before `next`; drop the rest.
    const keepFrom = Math.max(this.bufStart, this.next - Math.round(LEAD_S * this.fs));
    const drop = keepFrom - this.bufStart;
    if (drop > 0) {
      this.buf.copyWithin(0, 2 * drop, 2 * this.len);
      this.len -= drop;
      this.bufStart = keepFrom;
    }
    if (2 * (this.len + incoming) > this.buf.length) {
      const bigger = new Float32Array(2 * (this.len + incoming) * 2);
      bigger.set(this.buf.subarray(0, 2 * this.len));
      this.buf = bigger;
    }
  }

  private decodeWindow(winN: number): void {
    const view = this.buf.subarray(0, 2 * this.len);
    const from = this.next - this.bufStart;
    const tNow = this.next / this.fs;
    if ((!this.rx || tNow - this.lastFrameAt > 3) && tNow - this.lastAcquireAt >= this.acquireIntervalS) {
      // (Re)acquire on what is buffered around this window — at most every 2 s,
      // since on a quiet channel this is three demods of 1.5 s each.
      this.lastAcquireAt = tNow;
      const probe = view.subarray(2 * Math.max(0, from - Math.round(LEAD_S * this.fs)));
      const got = detectVoiceReceiver(probe, this.fs, this.force);
      if (got) {
        this.rx = got;
        this.lastFrameAt = tNow;
      }
    }
    if (this.rx) {
      const evs = findFrames(this.rx, view, this.fs, from, from + winN, this.bufStart / this.fs, this.cursor);
      for (const ev of evs) this.decoder.ingest(ev);
      if (evs.length) this.lastFrameAt = tNow;
    }
    this.next += winN;
    // Close calls that went quiet (no terminator heard).
    // (P25VoiceDecoder closes on the next frame; this handles dead air.)
    if (tNow - this.lastFrameAt > 1.5) this.decoder.finish(tNow);
  }
}
