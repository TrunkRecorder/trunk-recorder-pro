// Real-air check: voice heads on granted channels → freq-finder's P25LiveVoice.
// Usage: node --experimental-strip-types real_voice.ts file.cu8 centerHz f1 [f2 ...]
import { readFileSync, writeFileSync } from "node:fs";
import { FastConvChannelizer } from "./fastconv.ts";
const FF = "/Users/luke/Projects/SDR/freq-finder/web/src";
const { P25LiveVoice } = await import(`${FF}/dsp/p25Voice.ts`);
const [file, centerArg, ...freqs] = process.argv.slice(2);
const center = Number(centerArg), FS = 2_400_000;
const u8 = readFileSync(file);
const c = new FastConvChannelizer(FS, { fftSize: 16384, decim: 64, taps: 4097, cutoffHz: 7_000 });
const stats = freqs.map(() => ({ audio: 0, calls: [] as string[] }));
freqs.forEach((f, k) => {
  const live = new P25LiveVoice(c.outRate, {
    onAudio: (_ch: number, s: Float32Array) => (stats[k].audio += s.length),
    onCall: (call: any, ev: string) => { if (ev === "end") stats[k].calls.push(`TG ${call.tgid ?? call.talkgroup ?? "?"} src ${call.source ?? "?"} ${call.encrypted ? "ENC" : "clear"} ${(call.durationS ?? call.duration ?? 0).toFixed?.(1) ?? ""}`); },
  });
  c.addHead({ offsetHz: Number(f) - center, onOutput: (o) => live.push(o.slice()) });
});
const blk = new Float32Array(262144);
for (let o = 0; o < u8.length; o += blk.length) {
  const n = Math.min(blk.length, u8.length - o);
  for (let i = 0; i < n; i++) blk[i] = (u8[o + i] - 127.5) / 127.5;
  c.push(blk.subarray(0, n));
}
freqs.forEach((f, k) => console.log(`${(Number(f) / 1e6).toFixed(4)} MHz: ${(stats[k].audio / 8000).toFixed(1)} s audio; calls: ${stats[k].calls.join(" | ") || "(none ended)"}`));
