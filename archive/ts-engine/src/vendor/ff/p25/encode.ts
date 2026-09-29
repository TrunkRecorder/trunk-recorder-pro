// P25 Phase 1 control-channel modulator — the inverse of the decode stack, used
// by the sim (`SimulatedSource`) and the loopback tests. It builds real TSBK
// blocks (with the CRC-16), assembles a TSDU frame (sync + BCH-encoded NID +
// re-inserted status symbols + trellis-encoded blocks), and renders C4FM IQ.
//
// Because it is the exact inverse of the decoder, the loopback in
// `web/test/p25.test.ts` proves the whole chain (framing, status stripping,
// trellis, BCH, CRC, opcode field layout) self-consistently — the same honesty
// bar as the other synthetic decoders in this project.

import { appendCrc } from "./crc.ts";
import { encodeNid } from "./nid.ts";
import { trellisEncode } from "./trellis.ts";
import { P25_SYMBOL_RATE, SYNC_DIBITS } from "./c4fm.ts";

const DUID_TSDU = 0x7;

/** Set a field into a 96-bit block accumulator. */
function build(fields: { shift: number; value: number; width: number }[], opts: { opcode: number; mfid?: number; lastBlock?: boolean }): Uint8Array {
  let t = 0n;
  t |= BigInt(opts.opcode & 0x3f) << 88n;
  if (opts.lastBlock) t |= 1n << 95n;
  t |= BigInt(opts.mfid ?? 0) << 80n;
  for (const f of fields) t |= (BigInt(f.value) & ((1n << BigInt(f.width)) - 1n)) << BigInt(f.shift);
  const block = new Uint8Array(12);
  for (let i = 0; i < 10; i++) block[i] = Number((t >> BigInt((11 - i) * 8)) & 0xffn);
  return appendCrc(block);
}

export interface IdenSpec { iden: number; baseHz: number; stepHz: number; offsetHz: number; bwHz?: number; }
export function buildIdenUp(s: IdenSpec, lastBlock = false): Uint8Array {
  const freq = Math.round(s.baseHz / 5);
  const spac = Math.round(s.stepHz / 125);
  const bw = Math.round((s.bwHz ?? 12500) / 125);
  const mag = Math.round(Math.abs(s.offsetHz) / 250000) & 0xff;
  const toff0 = (s.offsetHz >= 0 ? (1 << 8) | mag : mag) & 0x1ff;
  return build(
    [
      { shift: 76, value: s.iden, width: 4 },
      { shift: 67, value: bw, width: 9 },
      { shift: 58, value: toff0, width: 9 },
      { shift: 48, value: spac, width: 10 },
      { shift: 16, value: freq, width: 32 },
    ],
    { opcode: 0x3d, lastBlock },
  );
}

export function buildNetSts(wacn: number, sysId: number, cc: number, lastBlock = false): Uint8Array {
  return build(
    [
      { shift: 52, value: wacn, width: 20 },
      { shift: 40, value: sysId, width: 12 },
      { shift: 24, value: cc, width: 16 },
    ],
    { opcode: 0x3b, lastBlock },
  );
}

export function buildRfssSts(sysId: number, rfss: number, site: number, cc: number, lastBlock = false): Uint8Array {
  return build(
    [
      { shift: 56, value: sysId, width: 12 },
      { shift: 48, value: rfss, width: 8 },
      { shift: 40, value: site, width: 8 },
      { shift: 24, value: cc, width: 16 },
    ],
    { opcode: 0x3a, lastBlock },
  );
}

export function buildAdjSts(rfss: number, site: number, ch: number, lastBlock = false): Uint8Array {
  return build(
    [
      { shift: 48, value: rfss, width: 8 },
      { shift: 40, value: site, width: 8 },
      { shift: 24, value: ch, width: 16 },
    ],
    { opcode: 0x3c, lastBlock },
  );
}

export function buildGrant(ch: number, group: number, source: number, lastBlock = false): Uint8Array {
  return build(
    [
      { shift: 56, value: ch, width: 16 },
      { shift: 40, value: group, width: 16 },
      { shift: 16, value: source, width: 24 },
    ],
    { opcode: 0x00, lastBlock },
  );
}

/**
 * Assemble one TSDU as a raw dibit stream: 24 sync dibits, then a bit-vector of
 * [sync(48) | NID(64) | trellis blocks(196 each)] with status dibits re-inserted
 * at every 36th raw-dibit position (the decoder strips exactly these).
 */
export function buildTsduDibits(nac: number, blocks: Uint8Array[]): Uint8Array {
  // stripped bit-vector, starting with the sync bits
  const strippedBits: number[] = [];
  for (const d of SYNC_DIBITS) { strippedBits.push((d >> 1) & 1, d & 1); }
  // NID (64 bits, MSB first)
  const nid = encodeNid(nac, DUID_TSDU);
  for (let i = 63; i >= 0; i--) strippedBits.push(Number((nid >> BigInt(i)) & 1n));
  // trellis-encoded blocks
  for (const blk of blocks) { const bits = trellisEncode(blk); for (const b of bits) strippedBits.push(b); }

  // stripped bits → stripped dibits
  const strippedDibits: number[] = [];
  for (let j = 0; j < strippedBits.length; j += 2) strippedDibits.push((strippedBits[j] << 1) | strippedBits[j + 1]);

  // re-insert status dibits at raw positions where (rawIdx+1)%36==0
  const raw: number[] = [];
  let si = 0;
  let rawIdx = 0;
  while (si < strippedDibits.length) {
    if ((rawIdx + 1) % 36 === 0) raw.push(0b01); // status symbol (+3); value irrelevant, it is stripped
    else raw.push(strippedDibits[si++]);
    rawIdx++;
  }
  return Uint8Array.from(raw);
}

/** C4FM-modulate a dibit stream to interleaved IQ. dibit → deviation:
 *  00→+600, 01→+1800, 10→-600, 11→-1800 Hz. */
export function dibitsToIq(
  dibits: ArrayLike<number>,
  sampleRateHz: number,
  opts: { carrierOffsetHz?: number; noise?: number; amp?: number } = {},
): Float32Array {
  const sps = sampleRateHz / P25_SYMBOL_RATE;
  const n = Math.floor(dibits.length * sps);
  const iq = new Float32Array(n * 2);
  const devFor = (d: number): number => (d === 0b00 ? 600 : d === 0b01 ? 1800 : d === 0b10 ? -600 : -1800);
  const amp = opts.amp ?? 1;
  const offset = opts.carrierOffsetHz ?? 0;
  const noise = opts.noise ?? 0;
  let phase = 0;
  for (let i = 0; i < n; i++) {
    const sym = Math.min(dibits.length - 1, Math.floor(i / sps));
    const dev = devFor(dibits[sym]) + offset;
    phase += (2 * Math.PI * dev) / sampleRateHz;
    iq[2 * i] = amp * Math.cos(phase) + (noise ? (Math.random() - 0.5) * noise : 0);
    iq[2 * i + 1] = amp * Math.sin(phase) + (noise ? (Math.random() - 0.5) * noise : 0);
  }
  return iq;
}

/**
 * The C4FM frequency pulse as TIA-102.BAAA specifies it: a raised-cosine
 * (α = 0.2) Nyquist filter cascaded with the "shaping filter" H(f) =
 * (πf/Rs)/sin(πf/Rs) — the inverse of a one-symbol integrate-and-dump. The
 * point of that cascade is the RECEIVER: integrating the discriminator over a
 * symbol (the boxcar in `demodC4fm`) undoes the shaping filter, leaving a
 * Nyquist pulse with no ISI at the symbol centres. Computed here as the inverse
 * Fourier transform of RC(f)/sinc(f/Rs), sampled at `sps` per symbol.
 */
function c4fmPulse(sps: number, span: number, alpha: number): Float64Array {
  const half = Math.ceil(span * sps);
  const h = new Float64Array(2 * half + 1);
  const B = (1 + alpha) / 2; // band edge, in units of the symbol rate
  const NF = 400;
  for (let i = -half; i <= half; i++) {
    const t = i / sps; // symbols
    let acc = 0;
    for (let k = 0; k < NF; k++) {
      const f = ((k + 0.5) / NF) * B;
      let rc = 1;
      if (f > (1 - alpha) / 2) rc = 0.5 * (1 + Math.cos((Math.PI / alpha) * (f - (1 - alpha) / 2)));
      const sinc = Math.sin(Math.PI * f) / (Math.PI * f);
      acc += (rc / sinc) * Math.cos(2 * Math.PI * f * t);
    }
    h[i + half] = (2 * acc * B) / NF; // even pulse, ∫ over ±B
  }
  return h;
}

/**
 * C4FM as a real P25 transmitter shapes it ({@link c4fmPulse}): each symbol's
 * ±600/±1800 Hz deviation through the RC × inverse-sinc pulse, then FM. It
 * occupies ~9 kHz and passes a 12.5 kHz channel filter intact, which the
 * rectangular-pulse {@link dibitsToIq} does not; this is what the voice sim
 * transmits.
 */
export function dibitsToC4fmShaped(
  dibits: ArrayLike<number>,
  sampleRateHz: number,
  opts: { carrierOffsetHz?: number; noise?: number; amp?: number; alpha?: number } = {},
): Float32Array {
  const sps = sampleRateHz / P25_SYMBOL_RATE;
  const span = 6;
  const pulse = c4fmPulse(sps, span, opts.alpha ?? 0.2);
  const half = (pulse.length - 1) / 2;
  // A one-symbol integrate-and-dump of the pulse must peak at 1 (the level):
  // normalise by the pulse's sum over one symbol about its centre.
  let norm = 0;
  for (let i = -Math.floor(sps / 2); i < Math.ceil(sps / 2); i++) norm += pulse[half + i];
  norm /= Math.round(sps);
  const n = Math.ceil((dibits.length + 1) * sps);
  const dev = new Float64Array(n);
  const devFor = (d: number): number => (d === 0b00 ? 600 : d === 0b01 ? 1800 : d === 0b10 ? -600 : -1800);
  for (let k = 0; k < dibits.length; k++) {
    const c = Math.round((k + 0.5) * sps);
    const d = devFor(dibits[k]) / norm;
    for (let j = -half; j <= half; j++) {
      const i = c + j;
      if (i >= 0 && i < n) dev[i] += d * pulse[half + j];
    }
  }
  const amp = opts.amp ?? 1;
  const noise = opts.noise ?? 0;
  const off = opts.carrierOffsetHz ?? 0;
  const iq = new Float32Array(2 * n);
  let phase = 0;
  for (let i = 0; i < n; i++) {
    phase += (2 * Math.PI * (dev[i] + off)) / sampleRateHz;
    iq[2 * i] = amp * Math.cos(phase) + (noise ? (Math.random() - 0.5) * noise : 0);
    iq[2 * i + 1] = amp * Math.sin(phase) + (noise ? (Math.random() - 0.5) * noise : 0);
  }
  return iq;
}
