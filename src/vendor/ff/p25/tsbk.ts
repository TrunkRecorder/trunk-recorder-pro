// P25 TSBK (Trunking Signalling Block) opcode parser. A decoded TSBK is 12 bytes
// (96 bits): byte0 = [last-block | protected | opcode(6)], byte1 = MFID, bytes
// 2..9 = arguments, bytes 10..11 = CRC. We parse the opcodes that describe the
// system, using the exact field layouts from op25's `tk_p25.py::decode_tsbk`
// (treating the 96-bit block as a big integer `t`, `opcode = (t>>88)&0x3f`).
//
// Only the system-describing opcodes are decoded richly; everything else is
// returned as `{ kind: "other" }` so the caller can still count it. Frequencies
// are NOT computed here (that needs the IDEN band-plan, which arrives in other
// TSBKs) — see `system.ts`.

export const TSBK = {
  GRP_V_CH_GRANT: 0x00,
  GRP_V_CH_GRANT_UPDT: 0x02,
  IDEN_UP_TDMA: 0x33,
  IDEN_UP_VU: 0x34,
  SCCB: 0x39,
  RFSS_STS_BCST: 0x3a,
  NET_STS_BCST: 0x3b,
  ADJ_STS_BCST: 0x3c,
  IDEN_UP: 0x3d,
} as const;

export interface IdenUp {
  kind: "iden";
  opcode: number;
  iden: number;
  /** base frequency, Hz */
  baseHz: number;
  /** channel spacing, Hz */
  stepHz: number;
  /** transmit (uplink) offset, Hz (signed) */
  offsetHz: number;
  /** channel bandwidth, Hz (0 if unknown) */
  bwHz: number;
  /** TDMA slots per carrier (1 for FDMA) */
  tdma: number;
}
export interface NetSts { kind: "netSts"; opcode: number; wacn: number; sysId: number; cc: number; }
export interface RfssSts { kind: "rfssSts"; opcode: number; sysId: number; rfss: number; site: number; cc: number; }
export interface AdjSts { kind: "adjSts"; opcode: number; rfss: number; site: number; ch: number; }
export interface Sccb { kind: "sccb"; opcode: number; rfss: number; site: number; ch1: number; ch2: number; }
export interface Grant { kind: "grant"; opcode: number; ch: number; group: number; source: number; }
export interface Other { kind: "other"; opcode: number; }

export type ParsedTsbk = IdenUp | NetSts | RfssSts | AdjSts | Sccb | Grant | Other;

/** Build the 96-bit integer from a 12-byte block. */
function toBig(block: Uint8Array): bigint {
  let t = 0n;
  for (let i = 0; i < 12; i++) t = (t << 8n) | BigInt(block[i]);
  return t;
}
const num = (t: bigint, shift: number, mask: number): number => Number((t >> BigInt(shift)) & BigInt(mask));

export function opcodeOf(block: Uint8Array): number {
  return block[0] & 0x3f;
}
export function isLastBlock(block: Uint8Array): boolean {
  return (block[0] >> 7) === 1;
}

export function parseTsbk(block: Uint8Array): ParsedTsbk {
  const t = toBig(block);
  const opcode = num(t, 88, 0x3f);
  switch (opcode) {
    case TSBK.IDEN_UP: {
      const iden = num(t, 76, 0xf);
      const bw = num(t, 67, 0x1ff);
      const toff0 = num(t, 58, 0x1ff);
      const spac = num(t, 48, 0x3ff);
      const freq = num(t, 16, 0xffffffff);
      const toffSign = (toff0 >> 8) & 1;
      let toff = toff0 & 0xff;
      if (toffSign === 0) toff = -toff;
      return {
        kind: "iden", opcode, iden,
        baseHz: freq * 5,
        stepHz: spac * 125,
        offsetHz: toff * 250000,
        bwHz: bw * 125,
        tdma: 1,
      };
    }
    case TSBK.IDEN_UP_VU: {
      const iden = num(t, 76, 0xf);
      const bwvu = num(t, 72, 0xf);
      const toff0 = num(t, 58, 0x3fff);
      const spac = num(t, 48, 0x3ff);
      const freq = num(t, 16, 0xffffffff);
      const toffSign = (toff0 >> 13) & 1;
      let toff = toff0 & 0x1fff;
      if (toffSign === 0) toff = -toff;
      return {
        kind: "iden", opcode, iden,
        baseHz: freq * 5,
        stepHz: spac * 125,
        offsetHz: toff * spac * 125,
        bwHz: bwvu === 0x04 ? 6250 : bwvu === 0x05 ? 12500 : 0,
        tdma: 1,
      };
    }
    case TSBK.IDEN_UP_TDMA: {
      const iden = num(t, 76, 0xf);
      const channelType = num(t, 72, 0xf);
      const toff0 = num(t, 58, 0x3fff);
      const spac = num(t, 48, 0x3ff);
      const f1 = num(t, 16, 0xffffffff);
      const toffSign = (toff0 >> 13) & 1;
      let toff = toff0 & 0x1fff;
      if (toffSign === 0) toff = -toff;
      const slotsPerCarrier = [1, 1, 1, 2, 4, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2];
      return {
        kind: "iden", opcode, iden,
        baseHz: f1 * 5,
        stepHz: spac * 125,
        offsetHz: toff * spac * 125,
        bwHz: 0,
        tdma: slotsPerCarrier[channelType],
      };
    }
    case TSBK.NET_STS_BCST:
      return { kind: "netSts", opcode, wacn: num(t, 52, 0xfffff), sysId: num(t, 40, 0xfff), cc: num(t, 24, 0xffff) };
    case TSBK.RFSS_STS_BCST:
      return { kind: "rfssSts", opcode, sysId: num(t, 56, 0xfff), rfss: num(t, 48, 0xff), site: num(t, 40, 0xff), cc: num(t, 24, 0xffff) };
    case TSBK.ADJ_STS_BCST:
      return { kind: "adjSts", opcode, rfss: num(t, 48, 0xff), site: num(t, 40, 0xff), ch: num(t, 24, 0xffff) };
    case TSBK.SCCB:
      return { kind: "sccb", opcode, rfss: num(t, 72, 0xff), site: num(t, 64, 0xff), ch1: num(t, 48, 0xffff), ch2: num(t, 24, 0xffff) };
    case TSBK.GRP_V_CH_GRANT: {
      const mfid = num(t, 80, 0xff);
      if (mfid !== 0 && mfid !== 0x90) return { kind: "other", opcode };
      return { kind: "grant", opcode, ch: num(t, 56, 0xffff), group: num(t, 40, 0xffff), source: num(t, 16, 0xffffff) };
    }
    case TSBK.GRP_V_CH_GRANT_UPDT: {
      const mfid = num(t, 80, 0xff);
      if (mfid !== 0) return { kind: "other", opcode };
      return { kind: "grant", opcode, ch: num(t, 64, 0xffff), group: num(t, 48, 0xffff), source: 0 };
    }
    default:
      return { kind: "other", opcode };
  }
}

/** Human-readable opcode name for the message label. */
export function opcodeName(opcode: number): string {
  switch (opcode) {
    case TSBK.GRP_V_CH_GRANT: return "GRP_V_CH_GRANT";
    case TSBK.GRP_V_CH_GRANT_UPDT: return "GRP_V_CH_GRANT_UPDT";
    case TSBK.IDEN_UP_TDMA: return "IDEN_UP_TDMA";
    case TSBK.IDEN_UP_VU: return "IDEN_UP_VU";
    case TSBK.SCCB: return "SCCB";
    case TSBK.RFSS_STS_BCST: return "RFSS_STS_BCST";
    case TSBK.NET_STS_BCST: return "NET_STS_BCST";
    case TSBK.ADJ_STS_BCST: return "ADJ_STS_BCST";
    case TSBK.IDEN_UP: return "IDEN_UP";
    default: return `OP_0x${opcode.toString(16).padStart(2, "0")}`;
  }
}
