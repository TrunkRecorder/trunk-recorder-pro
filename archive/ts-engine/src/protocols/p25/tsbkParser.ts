// P25 TSBK → TrunkMessage. A port of Trunk Recorder's P25Parser::decode_tsbk
// (trunk-recorder/systems/p25_parser.cc), opcode for opcode, including its
// frequency (IDEN) tables and TDMA slot mapping. Field positions are the same
// `bitset_shift_mask(tsbk, shift, mask)` calls: the 12-byte block is read as a
// 96-bit integer and `shift` counts from its least-significant bit.
//
// Deliberate differences from the C++:
//   - One parser instance per system, so there is no sys_num.
//   - `meta` strings are shorter (they are for the UI log, not Boost.Log).
//   - Adjacent-status (0x3c) returns a message typed "unknown" carrying the
//     frequency, instead of only logging it.

import { blankMessage, type TrunkMessage } from "../types.ts";

export interface FreqTable {
  id: number;
  offsetHz: number;
  stepHz: number;
  baseHz: number;
  phase2Tdma: boolean;
  slotsPerCarrier: number;
  bandwidthKhz: number;
}

function toBig(block: Uint8Array): bigint {
  let t = 0n;
  for (let i = 0; i < 12; i++) t = (t << 8n) | BigInt(block[i]);
  return t;
}

export class TsbkParser {
  readonly tables = new Map<number, FreqTable>();

  /** A custom table from config (Trunk Recorder's `customFrequencyTableFile`). */
  addFreqTable(t: FreqTable): void {
    this.tables.set(t.id, t);
  }

  channelToHz(ch: number): number {
    const t = this.tables.get((ch >> 12) & 0xf);
    if (!t) return 0;
    const channel = ch & 0xfff;
    return t.phase2Tdma ? t.baseHz + t.stepHz * Math.trunc(channel / t.slotsPerCarrier) : t.baseHz + t.stepHz * channel;
  }

  tdmaSlot(ch: number): number {
    const t = this.tables.get((ch >> 12) & 0xf);
    return t && t.phase2Tdma ? (ch & 0xfff) & 1 : -1;
  }

  private setSlot(m: TrunkMessage, ch: number): void {
    const s = this.tdmaSlot(ch);
    m.phase2Tdma = s >= 0;
    m.tdmaSlot = s >= 0 ? s : 0;
  }

  /** Parse one CRC-valid 12-byte TSBK. `nac` comes from the frame's NID. */
  parse(block: Uint8Array, nac: number, timeS: number): TrunkMessage[] {
    const t = toBig(block);
    const b = (shift: number, mask: number): number => Number((t >> BigInt(shift)) & BigInt(mask));
    const opcode = b(88, 0x3f);
    const out: TrunkMessage[] = [];
    const m = blankMessage(timeS, opcode, nac);
    const opts = (): void => {
      m.emergency = !!b(72, 0x80);
      m.encrypted = !!b(72, 0x40);
      m.duplex = !!b(72, 0x20);
      m.mode = !!b(72, 0x10);
      m.priority = b(72, 0x07);
    };
    const mhz = (hz: number) => (hz ? (hz / 1e6).toFixed(5) : "?");

    switch (opcode) {
      case 0x00: {
        if (b(80, 0xff) === 0x90) {
          // MOT_GRG_ADD_CMD — Motorola patch add
          m.type = "patch_add";
          m.patch = { sg: b(64, 0xffff), ga1: b(48, 0xffff), ga2: b(32, 0xffff), ga3: b(16, 0xffff) };
          m.meta = `Moto patch add sg ${m.patch.sg}: ${m.patch.ga1} ${m.patch.ga2} ${m.patch.ga3}`;
        } else {
          // GRP_V_CH_GRANT
          opts();
          const ch = b(56, 0xffff);
          m.type = "grant";
          m.freqHz = this.channelToHz(ch);
          m.talkgroup = b(40, 0xffff);
          m.source = b(16, 0xffffff);
          this.setSlot(m, ch);
          m.meta = `Grant TG ${m.talkgroup} ${mhz(m.freqHz)} MHz src ${m.source}${m.phase2Tdma ? ` slot ${m.tdmaSlot}` : ""}${m.encrypted ? " ENC" : ""}`;
        }
        break;
      }
      case 0x02: {
        if (b(80, 0xff) === 0x90) {
          // MOTOROLA_OSP_PATCH_GROUP_CHANNEL_GRANT
          opts();
          const ch = b(56, 0xffff);
          m.type = "grant";
          m.freqHz = this.channelToHz(ch);
          m.talkgroup = b(40, 0xffff);
          m.source = b(16, 0xffffff);
          this.setSlot(m, ch);
          m.meta = `Patch grant SG ${m.talkgroup} ${mhz(m.freqHz)} MHz src ${m.source}`;
        } else {
          // GRP_V_CH_GRANT_UPDT: two channel/group pairs
          const ch1 = b(64, 0xffff), ga1 = b(48, 0xffff), ch2 = b(32, 0xffff), ga2 = b(16, 0xffff);
          const f1 = this.channelToHz(ch1), f2 = this.channelToHz(ch2);
          m.type = "update";
          m.freqHz = f1;
          m.talkgroup = ga1;
          this.setSlot(m, ch1);
          m.meta = `Update TG ${ga1} ${mhz(f1)} MHz`;
          if (f1 !== f2 && ch2 !== 0xffff) {
            out.push({ ...m });
            m.freqHz = f2;
            m.talkgroup = ga2;
            this.setSlot(m, ch2);
            m.meta = `Update TG ${ga2} ${mhz(f2)} MHz`;
          }
        }
        break;
      }
      case 0x03: {
        if (b(80, 0xff) === 0x90) {
          // MOTOROLA_OSP_PATCH_GROUP_CHANNEL_GRANT_UPDATE
          const ch1 = b(64, 0xffff), sg1 = b(48, 0xffff), ch2 = b(32, 0xffff), sg2 = b(16, 0xffff);
          const f1 = this.channelToHz(ch1), f2 = this.channelToHz(ch2);
          m.type = "update";
          m.freqHz = f1;
          m.talkgroup = sg1;
          this.setSlot(m, ch1);
          m.meta = `Patch update SG ${sg1} ${mhz(f1)} MHz`;
          if (f1 !== f2) {
            out.push({ ...m });
            m.freqHz = f2;
            m.talkgroup = sg2;
            this.setSlot(m, ch2);
            m.meta = `Patch update SG ${sg2} ${mhz(f2)} MHz`;
          }
        } else {
          // GRP_V_CH_GRANT_UPDT_EXP
          m.emergency = !!b(72, 0x80);
          m.encrypted = !!b(72, 0x40);
          const ch1 = b(48, 0xffff);
          m.type = "update";
          m.freqHz = this.channelToHz(ch1);
          m.talkgroup = b(16, 0xffff);
          this.setSlot(m, ch1);
          m.meta = `Explicit update TG ${m.talkgroup} ${mhz(m.freqHz)} MHz`;
        }
        break;
      }
      case 0x04: {
        // UU_V_CH_GRANT
        opts();
        const ch = b(64, 0xffff);
        m.type = "uu_v_grant";
        m.freqHz = this.channelToHz(ch);
        m.talkgroup = b(40, 0xffffff);
        m.source = b(16, 0xffffff);
        this.setSlot(m, ch);
        m.meta = `Unit-to-unit grant ${m.source} → ${m.talkgroup} ${mhz(m.freqHz)} MHz`;
        break;
      }
      case 0x05: {
        if (b(80, 0xff) === 0x90) {
          m.meta = "MOTOROLA_OSP_TRAFFIC_CHANNEL_ID";
        } else {
          opts();
          m.type = "uu_ans_req";
          m.source = b(16, 0xffffff);
          m.talkgroup = b(40, 0xffffff);
          m.meta = `Unit-to-unit answer request ${m.talkgroup} → ${m.source}`;
        }
        break;
      }
      case 0x06: {
        // UU_V_CH_GRANT_UPDT
        const ch = b(64, 0xffff);
        m.type = "uu_v_update";
        m.freqHz = this.channelToHz(ch);
        m.talkgroup = b(40, 0xffffff);
        m.source = b(16, 0xffffff);
        this.setSlot(m, ch);
        m.meta = `Unit-to-unit update ${m.source} → ${m.talkgroup} ${mhz(m.freqHz)} MHz`;
        break;
      }
      case 0x14: {
        // SNDCP data channel grant
        m.emergency = !!b(72, 0x80);
        m.encrypted = !!b(72, 0x40);
        m.duplex = !!b(72, 0x20);
        m.mode = !!b(72, 0x10);
        m.type = "data_grant";
        m.source = b(16, 0xffffff);
        m.freqHz = this.channelToHz(b(56, 0xffff));
        m.meta = `Data grant src ${m.source} ${mhz(m.freqHz)} MHz`;
        break;
      }
      case 0x1f: {
        m.type = "call_alert";
        m.source = b(16, 0xffffff);
        m.talkgroup = b(40, 0xffffff);
        m.meta = `Call alert ${m.source} → ${m.talkgroup}`;
        break;
      }
      case 0x20: {
        m.type = "acknowledge";
        m.talkgroup = b(40, 0xffff);
        m.source = b(16, 0xffffff);
        m.meta = `Acknowledge src ${m.source}`;
        break;
      }
      case 0x28: {
        m.type = "affiliation";
        m.source = b(16, 0xffffff);
        m.talkgroup = b(40, 0xffff);
        m.meta = `Affiliation ${m.source} → TG ${m.talkgroup}`;
        break;
      }
      case 0x29:
      case 0x39: {
        // Secondary control channel broadcast (explicit / implicit)
        const f1 = this.channelToHz(b(48, 0xffff));
        const f2 = this.channelToHz(b(24, 0xffff));
        m.meta = `Secondary CC rfss ${b(72, 0xff)} site ${b(64, 0xff)}: ${mhz(f1)} / ${mhz(f2)} MHz`;
        if (f1 && f2) {
          m.type = "control_channel";
          m.freqHz = f1;
          out.push({ ...m });
          m.freqHz = f2;
        }
        break;
      }
      case 0x2b: {
        m.type = "location";
        m.talkgroup = b(56, 0xffff);
        m.source = b(16, 0xffffff);
        m.meta = `Location registration ${m.source} TG ${m.talkgroup}`;
        break;
      }
      case 0x2c: {
        m.type = "registration";
        m.source = b(40, 0xffffff);
        m.meta = `Registration ${m.source}`;
        break;
      }
      case 0x2f: {
        m.type = "deregistration";
        m.source = b(16, 0xffffff);
        m.meta = `Deregistration ${m.source}`;
        break;
      }
      case 0x30: {
        if (b(80, 0xff) === 0xa4) {
          // GRG_EXENC_CMD (M/A-COM patch)
          const ga = b(16, 0xffffff) & 0xffff;
          const sg = b(56, 0xffff);
          m.type = b(77, 0x01) === 1 ? "patch_add" : "patch_delete";
          m.patch = { sg, ga1: ga, ga2: ga, ga3: ga };
          m.meta = `M/A-COM patch ${m.type === "patch_add" ? "add" : "delete"} sg ${sg} TG ${ga}`;
        } else {
          m.meta = "TDMA sync broadcast";
        }
        break;
      }
      case 0x33: {
        // IDEN_UP_TDMA
        if (b(80, 0xff) === 0) {
          const iden = b(76, 0xf);
          const channelType = b(72, 0xf);
          const toff0 = b(58, 0x3fff);
          const spac = b(48, 0x3ff);
          let toff = toff0 & 0x1fff;
          if (((toff0 >> 13) & 1) === 0) toff = -toff;
          const slots = [1, 1, 1, 2, 4, 2][channelType] ?? 1;
          this.addFreqTable({
            id: iden,
            offsetHz: toff * spac * 125,
            stepHz: spac * 125,
            baseHz: b(16, 0xffffffff) * 5,
            phase2Tdma: slots > 1,
            slotsPerCarrier: slots,
            bandwidthKhz: 6.25,
          });
          m.meta = `IDEN_UP_TDMA ${iden}: base ${mhz(b(16, 0xffffffff) * 5)} MHz, step ${spac * 125} Hz, ${slots} slot(s)`;
        }
        break;
      }
      case 0x34: {
        // IDEN_UP_VU
        const iden = b(76, 0xf);
        const bwvu = b(72, 0xf);
        const toff0 = b(58, 0x3fff);
        const spac = b(48, 0x3ff);
        let toff = toff0 & 0x1fff;
        if (((toff0 >> 13) & 1) === 0) toff = -toff;
        this.addFreqTable({
          id: iden,
          offsetHz: toff * spac * 125,
          stepHz: spac * 125,
          baseHz: b(16, 0xffffffff) * 5,
          phase2Tdma: false,
          slotsPerCarrier: 0,
          bandwidthKhz: bwvu === 4 ? 6.25 : bwvu === 5 ? 12.5 : 0,
        });
        m.meta = `IDEN_UP_VU ${iden}: base ${mhz(b(16, 0xffffffff) * 5)} MHz, step ${spac * 125} Hz`;
        break;
      }
      case 0x3a: {
        // RFSS status
        m.type = "sysid";
        m.sysId = b(56, 0xfff);
        m.rfss = b(48, 0xff);
        m.site = b(40, 0xff);
        m.meta = `RFSS status sysid ${m.sysId.toString(16)} rfss ${m.rfss} site ${m.site}`;
        break;
      }
      case 0x3b: {
        // Network status
        const f1 = this.channelToHz(b(24, 0xffff));
        m.meta = `Network status wacn ${b(52, 0xfffff).toString(16)} sysid ${b(40, 0xfff).toString(16)} CC ${mhz(f1)} MHz`;
        if (f1) {
          m.type = "status";
          m.wacn = b(52, 0xfffff);
          m.sysId = b(40, 0xfff);
          m.freqHz = f1;
        }
        break;
      }
      case 0x3c: {
        // Adjacent status
        m.rfss = b(48, 0xff);
        m.site = b(40, 0xff);
        m.freqHz = this.channelToHz(b(24, 0xffff));
        m.meta = `Adjacent site rfss ${m.rfss} site ${m.site} CC ${mhz(m.freqHz)} MHz`;
        break;
      }
      case 0x3d: {
        // IDEN_UP
        const iden = b(76, 0xf);
        const bw = b(67, 0x1ff);
        const toff0 = b(58, 0x1ff);
        const spac = b(48, 0x3ff);
        let toff = toff0 & 0xff;
        if (((toff0 >> 8) & 1) === 0) toff = -toff;
        this.addFreqTable({
          id: iden,
          offsetHz: toff * 250_000,
          stepHz: spac * 125,
          baseHz: b(16, 0xffffffff) * 5,
          phase2Tdma: false,
          slotsPerCarrier: 1,
          bandwidthKhz: bw * 0.125,
        });
        m.meta = `IDEN_UP ${iden}: base ${mhz(b(16, 0xffffffff) * 5)} MHz, step ${spac * 125} Hz`;
        break;
      }
      default:
        m.meta = `TSBK opcode 0x${opcode.toString(16).padStart(2, "0")}`;
        break;
    }
    out.push(m);
    return out;
  }
}
