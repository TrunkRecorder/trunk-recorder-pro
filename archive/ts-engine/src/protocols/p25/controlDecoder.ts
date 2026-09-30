// Streaming P25 Phase 1 control-channel decoder.
//
// freq-finder's decodeP25Into decodes one capture window and dedupes messages
// for a summary; trunk following needs the opposite — every TSBK, in order, with
// its time, a fraction of a second after it was sent. So this runs the same
// frame chain (sync → status strip → NID/BCH → trellis → CRC) over overlapping
// windows of a live stream:
//
//   window k covers [next − LEAD, next + WIN + TAIL); a frame belongs to the
//   window its sync STARTS in, within [next, next + WIN). LEAD lets the demod's
//   timing/carrier loops settle before the claimed span; TAIL (> one 3-block
//   TSDU, ~75 ms) lets a frame that starts late in the span finish.
//
// Latency is WIN + TAIL ≈ 0.35 s. The modulation (C4FM vs simulcast CQPSK/LSM)
// is measured per window and voted over the run; while undecided both receivers
// run and the one that finds more frame syncs wins, as in freq-finder.

import { demodC4fmTracked, dibitsToBits, invertDibit, P25_SYMBOL_RATE, SYNC_DIBITS } from "../../vendor/ff/p25/c4fm.ts";
import { decodeNid } from "../../vendor/ff/p25/nid.ts";
import { trellisDecode } from "../../vendor/ff/p25/trellis.ts";
import { tsbkCrcOk } from "../../vendor/ff/p25/crc.ts";
import { isLastBlock } from "../../vendor/ff/p25/tsbk.ts";
import { measureModulation } from "../../vendor/ff/p25/modulation.ts";
import { measureSignalCenter } from "../../vendor/ff/p25/tuning.ts";
import { demodulate } from "../../vendor/ff/demod.ts";
import type { ControlDecoder, ControlStats, SystemIdentity, TrunkMessage } from "../types.ts";
import { TsbkParser } from "./tsbkParser.ts";

export type P25Modulation = "auto" | "fsk4" | "qpsk";

const WIN_S = 0.25;
const LEAD_S = 0.1;
const TAIL_S = 0.1;
const SYNC_LEN = 24;
const MIN_SYNC_MATCH = 20;
const DUID_TSDU = 0x7;
const MAX_BLOCKS = 3;
const MAX_FRAME_DIBITS = SYNC_LEN + 32 + 98 * MAX_BLOCKS + 16;
/** Windows of modulation evidence before the vote is trusted. */
const MOD_VOTES = 8;

function syncScore(d: Uint8Array, pos: number, invert: boolean): number {
  let match = 0;
  for (let i = 0; i < SYNC_LEN; i++) if ((invert ? invertDibit(d[pos + i]) : d[pos + i]) === SYNC_DIBITS[i]) match++;
  return match;
}

function syncHits(d: Uint8Array, invert: boolean): number {
  let hits = 0;
  for (let p = 0; p + SYNC_LEN <= d.length; p++) {
    if (syncScore(d, p, invert) >= MIN_SYNC_MATCH) {
      hits++;
      p += SYNC_LEN - 1;
    }
  }
  return hits;
}

interface Symbols {
  dibits: Uint8Array;
  /** Sample index (within the window) of each symbol. */
  instants: Float32Array;
}

export class P25ControlDecoder implements ControlDecoder {
  readonly parser = new TsbkParser();
  private buf: Float32Array;
  private len = 0; // complex samples in buf
  private bufStart = 0; // absolute sample index of buf[0]
  private next = 0; // absolute sample index where the next claimed span starts
  private readonly win: number;
  private readonly lead: number;
  private readonly tail: number;
  private good = 0;
  private bad = 0;
  private qpskVotes = 0;
  private fsk4Votes = 0;
  private readonly nacCounts = new Map<number, number>();
  private readonly ident: SystemIdentity = { nac: null, wacn: null, sysId: null, rfss: null, site: null };
  private readonly rate: number;
  private readonly onMessages: (msgs: TrunkMessage[]) => void;
  private readonly forced: P25Modulation;

  constructor(rate: number, onMessages: (msgs: TrunkMessage[]) => void, modulation: P25Modulation = "auto") {
    this.rate = rate;
    this.onMessages = onMessages;
    this.forced = modulation;
    this.win = Math.round(WIN_S * rate);
    this.lead = Math.round(LEAD_S * rate);
    this.tail = Math.round(TAIL_S * rate);
    this.buf = new Float32Array(2 * (this.lead + this.win + this.tail) * 4);
  }

  push(iq: Float32Array): void {
    const n = iq.length >> 1;
    if (2 * (this.len + n) > this.buf.length) this.compact(n);
    this.buf.set(iq, 2 * this.len);
    this.len += n;
    while (this.bufStart + this.len - this.next >= this.win + this.tail) this.decodeWindow();
  }

  stats(): ControlStats {
    const mod = this.modulation();
    return { good: this.good, bad: this.bad, modulation: mod === "qpsk" ? "CQPSK" : mod === "fsk4" ? "C4FM" : null };
  }

  identity(): SystemIdentity {
    return { ...this.ident };
  }

  /** The decided physical layer, or "auto" while the evidence is thin. */
  modulation(): P25Modulation {
    if (this.forced !== "auto") return this.forced;
    const total = this.qpskVotes + this.fsk4Votes;
    if (total < MOD_VOTES) return "auto";
    if (this.qpskVotes >= 0.75 * total) return "qpsk";
    if (this.fsk4Votes >= 0.75 * total) return "fsk4";
    return "auto";
  }

  private compact(incoming: number): void {
    const keepFrom = Math.max(this.bufStart, this.next - this.lead);
    const drop = keepFrom - this.bufStart;
    const keep = this.len - drop;
    const need = 2 * (keep + incoming);
    if (need > this.buf.length) {
      const bigger = new Float32Array(Math.max(need, this.buf.length * 2));
      bigger.set(this.buf.subarray(2 * drop, 2 * this.len));
      this.buf = bigger;
    } else {
      this.buf.copyWithin(0, 2 * drop, 2 * this.len);
    }
    this.len = keep;
    this.bufStart = keepFrom;
  }

  private symbols(iq: Float32Array, kind: "fsk4" | "qpsk"): Symbols {
    if (kind === "fsk4") {
      const r = demodC4fmTracked(iq, this.rate);
      return { dibits: r.dibits, instants: r.instants };
    }
    const center = measureSignalCenter(iq, this.rate);
    const r = demodulate(iq, this.rate, "pi4dqpsk", P25_SYMBOL_RATE, { carrierOffsetHz: center?.offsetHz ?? 0 });
    return { dibits: r.symbols, instants: r.sampleInstants };
  }

  private decodeWindow(): void {
    const from = Math.max(this.bufStart, this.next - this.lead);
    const to = this.next + this.win + this.tail;
    const iq = this.buf.subarray(2 * (from - this.bufStart), 2 * (to - this.bufStart));
    const claimLo = this.next - from;
    const claimHi = claimLo + this.win;
    this.next += this.win;

    let kind = this.modulation();
    let sym: Symbols;
    if (kind === "auto") {
      const m = measureModulation(iq, this.rate);
      if (m?.kind === "qpsk") kind = "qpsk";
      else if (m?.kind === "fsk4") kind = "fsk4";
    }
    if (kind === "auto") {
      const a = this.symbols(iq, "fsk4");
      const b = this.symbols(iq, "qpsk");
      const ha = Math.max(syncHits(a.dibits, false), syncHits(a.dibits, true));
      const hb = Math.max(syncHits(b.dibits, false), syncHits(b.dibits, true));
      if (hb > ha) this.qpskVotes++;
      else if (ha > hb) this.fsk4Votes++;
      sym = hb > ha ? b : a;
    } else {
      sym = this.symbols(iq, kind);
      if (this.forced === "auto" && this.modulation() === "auto") {
        if (kind === "qpsk") this.qpskVotes++;
        else this.fsk4Votes++;
      }
    }

    const d = sym.dibits;
    if (d.length < SYNC_LEN + 32) return;
    if (syncHits(d, true) > syncHits(d, false)) for (let i = 0; i < d.length; i++) d[i] = invertDibit(d[i]);

    const syncs: number[] = [];
    for (let p = 0; p + SYNC_LEN <= d.length; p++) {
      if (syncScore(d, p, false) >= MIN_SYNC_MATCH) {
        syncs.push(p);
        p += SYNC_LEN - 1;
      }
    }

    const msgs: TrunkMessage[] = [];
    for (let s = 0; s < syncs.length; s++) {
      const pos = syncs[s];
      const at = sym.instants[pos];
      if (!(at >= claimLo && at < claimHi)) continue; // another window's frame
      const end = Math.min(s + 1 < syncs.length ? syncs[s + 1] : d.length, pos + MAX_FRAME_DIBITS, d.length);
      const kept: number[] = [];
      for (let li = 0; pos + li < end; li++) if ((li + 1) % 36 !== 0) kept.push(d[pos + li]);
      const bits = dibitsToBits(kept);
      if (bits.length < 48 + 64 + 196) continue;
      let nidAcc = 0n;
      for (let i = 0; i < 64; i++) nidAcc = (nidAcc << 1n) | BigInt(bits[48 + i]);
      const nid = decodeNid(nidAcc);
      if (!nid) continue;
      this.noteNac(nid.nac);
      if (nid.duid !== DUID_TSDU) continue;
      const timeS = (from + at) / this.rate;
      const blocks = Math.min(MAX_BLOCKS, Math.floor((bits.length - 112) / 196));
      for (let b = 0; b < blocks; b++) {
        const block = trellisDecode(bits, 112 + b * 196);
        if (!block) {
          this.bad++;
          break;
        }
        if (!tsbkCrcOk(block)) {
          this.bad++;
          continue;
        }
        this.good++;
        for (const m of this.parser.parse(block, nid.nac, timeS)) {
          this.noteIdentity(m);
          msgs.push(m);
        }
        if (isLastBlock(block)) break;
      }
    }
    if (msgs.length) this.onMessages(msgs);
  }

  private noteNac(nac: number): void {
    const c = (this.nacCounts.get(nac) ?? 0) + 1;
    this.nacCounts.set(nac, c);
    let best = this.ident.nac;
    let bestC = best === null ? 0 : (this.nacCounts.get(best) ?? 0);
    if (c > bestC) {
      best = nac;
      bestC = c;
    }
    this.ident.nac = best;
  }

  private noteIdentity(m: TrunkMessage): void {
    if (m.type === "status") {
      this.ident.wacn = m.wacn;
      this.ident.sysId = m.sysId;
    } else if (m.type === "sysid") {
      this.ident.sysId = m.sysId;
      this.ident.rfss = m.rfss;
      this.ident.site = m.site;
    }
  }
}
