// Real-air check: an rtl_sdr .cu8 wideband capture → fastconv head on the CC →
// freq-finder's decodeP25OTA. Usage: node --experimental-strip-types real_cc.ts file.cu8 centerHz ccHz
import { readFileSync } from "node:fs";
import { FastConvChannelizer } from "./fastconv.ts";
const FF = "/Users/luke/Projects/SDR/freq-finder/web/src";
const { decodeP25OTA } = await import(`${FF}/dsp/p25.ts`);
const [file, centerArg, ccArg] = process.argv.slice(2);
const center = Number(centerArg), cc = Number(ccArg), FS = 2_400_000;
const u8 = readFileSync(file);
const c = new FastConvChannelizer(FS, { fftSize: 16384, decim: 64, taps: 4097, cutoffHz: 7_000 });
const out: number[] = [];
c.addHead({ offsetHz: cc - center, onOutput: (o) => { for (const x of o) out.push(x); } });
const blk = new Float32Array(262144);
for (let o = 0; o < u8.length; o += blk.length) {
  const n = Math.min(blk.length, u8.length - o);
  for (let i = 0; i < n; i++) blk[i] = (u8[o + i] - 127.5) / 127.5;
  c.push(blk.subarray(0, n));
}
const r = decodeP25OTA(Float32Array.from(out), c.outRate);
console.log(r.info);
console.log(JSON.stringify({ ccs: r.system.controlChannelsHz ?? r.system.controlChannels, grants: r.system.voiceGrants, tuning: r.system.tuning, modulation: r.system.modulation?.kind }, null, 0));
