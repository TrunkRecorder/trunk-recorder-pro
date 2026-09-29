// Verifies and times the fast-convolution channelizer (fastconv.ts):
//  1. a tone lands at the right residual frequency and level; a far channel rejects it
//  2. END TO END: a 2.4 MSPS capture holding a P25 control channel and a P25 voice
//     call is channelized, and both decode through freq-finder's decoders
//  3. cost vs number of simultaneous channels
// Run: node --experimental-strip-types bench_fastconv.ts [freq-finder/web/src]
import { performance } from "node:perf_hooks";
import { FastConvChannelizer } from "./fastconv.ts";

const FF = process.argv[2] ?? "/Users/luke/Projects/SDR/freq-finder/web/src";
const { P25LiveVoice } = await import(`${FF}/dsp/p25Voice.ts`);
const { decodeP25OTA } = await import(`${FF}/dsp/p25.ts`);
const enc = await import(`${FF}/dsp/p25/encode.ts`);
const sim = await import(`${FF}/dsp/p25/voiceSim.ts`);

const FS = 2_400_000;
const OPTS = { fftSize: 16384, decim: 64, taps: 4097, cutoffHz: 7_000 };
const make = () => new FastConvChannelizer(FS, OPTS);
const probe = make();
console.log(`config: N=${probe.n} forward FFT, M=${probe.m} per-head IFFT, L=${probe.l} new samples/block, out ${probe.outRate} SPS`);

// ── 1. tone ──────────────────────────────────────────────────────────────────
{
  const f = 312_550, n = FS; // 1 s
  const iq = new Float32Array(2 * n);
  for (let i = 0; i < n; i++) { iq[2 * i] = Math.cos((2 * Math.PI * f * i) / FS); iq[2 * i + 1] = Math.sin((2 * Math.PI * f * i) / FS); }
  const got: Record<string, number[]> = { on: [], off: [] };
  const c = make();
  c.addHead({ offsetHz: 312_500, onOutput: (o) => got.on.push(...o) });
  c.addHead({ offsetHz: -600_000, onOutput: (o) => got.off.push(...o) });
  c.push(iq);
  const tail = (a: number[]) => a.slice(a.length / 2); // skip filter start-up
  const on = tail(got.on), off = tail(got.off);
  let p = 0, q = 0, dph = 0;
  for (let i = 0; i < on.length; i += 2) p += on[i] ** 2 + on[i + 1] ** 2;
  for (let i = 0; i < off.length; i += 2) q += off[i] ** 2 + off[i + 1] ** 2;
  for (let i = 2; i < on.length; i += 2) dph += Math.atan2(on[i + 1] * on[i - 2] - on[i] * on[i - 1], on[i] * on[i - 2] + on[i + 1] * on[i - 1]);
  const hz = (dph / (on.length / 2 - 1)) * probe.outRate / (2 * Math.PI);
  console.log(`tone: in-channel ${(10 * Math.log10(p / (on.length / 2))).toFixed(2)} dB (want 0), at ${hz.toFixed(1)} Hz (want +50.0); 912 kHz away ${(10 * Math.log10(q / (off.length / 2) + 1e-30)).toFixed(1)} dB`);
}

// ── 2. wideband P25: control channel + voice call through the channelizer ────
const CC_HZ = -600_000, VOICE_HZ = 312_500 + 37; // +37 Hz: deliberately off-bin
function wideband(): Float32Array {
  const frames = sim.melody(18 * 12, [262, 330, 392, 523], 25, (hz: number) => sim.packImbe(sim.imbeB0ForHz(hz)));
  const vDib = sim.p1CallDibits({ nac: 0x4d8, tgid: 101, source: 555, frames });
  const v48 = enc.dibitsToC4fmShaped(vDib, 48_000, {}); // spec-shaped C4FM at 48 k
  // FM-upsample the voice to 2.4 MSPS: instantaneous frequency, linearly interpolated.
  const nV = v48.length / 2, R = FS / 48_000;
  const inst = new Float64Array(nV);
  for (let i = 1; i < nV; i++) inst[i] = Math.atan2(v48[2 * i + 1] * v48[2 * i - 2] - v48[2 * i] * v48[2 * i - 1], v48[2 * i] * v48[2 * i - 2] + v48[2 * i + 1] * v48[2 * i - 1]) / R;
  const n = Math.floor(nV * R);
  const cc = [
    enc.buildIdenUp({ iden: 1, baseHz: 762_000_000, stepHz: 12_500, offsetHz: 30_000_000 }),
    enc.buildNetSts(0xbee00, 0x123, 80), enc.buildRfssSts(0x123, 1, 3, 80), enc.buildGrant(100, 101, 555, true),
  ];
  const one = enc.buildTsduDibits(0x4d8, cc);
  const ccDib = new Uint8Array(one.length * Math.ceil((n / FS) * 4800 / one.length + 1));
  for (let o = 0; o + one.length <= ccDib.length; o += one.length) ccDib.set(one, o);
  const ccIq = enc.dibitsToIq(ccDib.subarray(0, Math.ceil((n / FS) * 4800) + 2), FS, { carrierOffsetHz: CC_HZ, amp: 0.3 });
  const out = new Float32Array(2 * n);
  let ph = 0;
  for (let i = 0; i < n; i++) {
    const j = i / R, j0 = Math.floor(j), fr = j - j0;
    ph += (inst[j0] ?? 0) * (1 - fr) + (inst[j0 + 1] ?? 0) * fr + (2 * Math.PI * VOICE_HZ) / FS;
    out[2 * i] = 0.3 * Math.cos(ph) + ccIq[2 * i] + (Math.random() - 0.5) * 0.05;
    out[2 * i + 1] = 0.3 * Math.sin(ph) + ccIq[2 * i + 1] + (Math.random() - 0.5) * 0.05;
  }
  return out;
}
const wb = wideband();
const airS = wb.length / 2 / FS;
{
  const c = make();
  const ccBuf: number[] = [];
  let audio = 0, calls = 0, tg = -1;
  const live = new P25LiveVoice(c.outRate, {
    onAudio: (_c: number, s: Float32Array) => (audio += s.length),
    onCall: (call: { talkgroup?: number; tgid?: number }, ev: string) => { if (ev === "start") calls++; tg = (call as any).tgid ?? (call as any).talkgroup ?? tg; },
  });
  c.addHead({ offsetHz: CC_HZ, onOutput: (o) => { for (const x of o) ccBuf.push(x); } });
  c.addHead({ offsetHz: VOICE_HZ, onOutput: (o) => live.push(o.slice()) });
  for (let o = 0; o < wb.length; o += 131072) c.push(wb.subarray(o, Math.min(wb.length, o + 131072)));
  const r = decodeP25OTA(Float32Array.from(ccBuf), c.outRate);
  console.log(`e2e ${airS.toFixed(1)} s of 2.4 MSPS air -> control channel: ${r.info}`);
  console.log(`e2e voice: ${calls} call(s), TG ${tg}, ${(audio / 8000).toFixed(1)} s of audio (call is ${(18 * 12 * 0.02).toFixed(1)} s of voice)`);
}

// ── 3. cost vs channel count ─────────────────────────────────────────────────
for (const heads of [1, 8, 16, 32, 64]) {
  const c = make();
  for (let h = 0; h < heads; h++) c.addHead({ offsetHz: -1_000_000 + h * 30_000, onOutput: () => {} });
  c.push(wb.subarray(0, 2 * 20 * c.l)); // warm-up
  const t0 = performance.now();
  for (let o = 0; o < wb.length; o += 131072) c.push(wb.subarray(o, Math.min(wb.length, o + 131072)));
  const ms = performance.now() - t0;
  console.log(`fast-conv channelizer, ${String(heads).padStart(2)} channels: ${((ms / 1000 / airS) * 100).toFixed(2)}% of one core (${((ms / 1000 / airS) * 100 / heads).toFixed(3)}% per channel)`);
}
