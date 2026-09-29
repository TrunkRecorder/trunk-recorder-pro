// Feasibility benchmark: can one browser core (V8) run a Trunk-Recorder-shaped
// workload — channelize 2.4 MSPS into N channels, decode a P25 control channel,
// and decode + vocode N P25 Phase 1 voice channels — in real time?
//
// Uses freq-finder's production TypeScript DSP unchanged (op25/mbelib ports).
// Run: node --experimental-strip-types bench.ts [freq-finder path]
import { performance } from "node:perf_hooks";

const FF = process.argv[2] ?? "/Users/luke/Projects/SDR/freq-finder/web/src";
const { ComplexFirDecimator, designLowpass } = await import(`${FF}/dsp/filters.ts`);
const { P25LiveVoice } = await import(`${FF}/dsp/p25Voice.ts`);
const { decodeP25OTA } = await import(`${FF}/dsp/p25.ts`);
const enc = await import(`${FF}/dsp/p25/encode.ts`);
const sim = await import(`${FF}/dsp/p25/voiceSim.ts`);

const FS = 2_400_000;
const CH_FS = 48_000;
const DECIM = FS / CH_FS; // 50
const BLOCK = 65536; // one ~27 ms USB transfer

function fmt(ms: number, audioS: number) {
  return `${ms.toFixed(1)} ms for ${audioS.toFixed(1)} s of air -> ${((ms / 1000 / audioS) * 100).toFixed(2)}% of one core`;
}

// ── 1. Per-channel channelizer: NCO mix + decimating FIR, 2.4 MSPS -> 48 kSPS ─
{
  const SECONDS = 4;
  const n = FS * SECONDS;
  const inI = new Float32Array(BLOCK), inQ = new Float32Array(BLOCK);
  for (let i = 0; i < BLOCK; i++) { inI[i] = Math.random() - 0.5; inQ[i] = Math.random() - 0.5; }
  for (const taps of [101, 201]) {
    const lp = designLowpass(taps, 7_000 / FS);
    const fir = new ComplexFirDecimator(lp, DECIM);
    const mI = new Float32Array(BLOCK), mQ = new Float32Array(BLOCK);
    const w = (2 * Math.PI * 312_500) / FS;
    let ph = 0;
    // warm-up
    for (let b = 0; b < 20; b++) fir.process(inI, inQ, BLOCK);
    const t0 = performance.now();
    for (let done = 0; done < n; done += BLOCK) {
      for (let i = 0; i < BLOCK; i++) {
        const c = Math.cos(ph), s = Math.sin(ph);
        mI[i] = inI[i] * c - inQ[i] * s;
        mQ[i] = inI[i] * s + inQ[i] * c;
        ph += w;
      }
      ph %= 2 * Math.PI;
      fir.process(mI, mQ, BLOCK);
    }
    console.log(`channelizer (${taps} taps, NCO+FIR, 1 channel): ${fmt(performance.now() - t0, SECONDS)}`);
  }
}

// ── 2. P25 Phase 1 voice: 48 kSPS IQ -> call tracking -> IMBE -> 8 kHz audio ─
{
  const frames = sim.melody(18 * 30, [262, 330, 392, 523], 25, (hz: number) => sim.packImbe(sim.imbeB0ForHz(hz)));
  const dib = sim.p1CallDibits({ nac: 0x4d8, tgid: 101, source: 555, frames });
  const iq = enc.dibitsToC4fmShaped(dib, CH_FS, { carrierOffsetHz: 300, noise: 0.05 });
  const secs = iq.length / 2 / CH_FS;
  let audioSamples = 0, calls = 0;
  const live = new P25LiveVoice(CH_FS, { onAudio: (_c: number, s: Float32Array) => (audioSamples += s.length), onCall: (_c: unknown, ev: string) => { if (ev === "start") calls++; } });
  const chunk = 2 * Math.round(0.025 * CH_FS);
  const t0 = performance.now();
  for (let o = 0; o < iq.length; o += chunk) live.push(iq.subarray(o, Math.min(iq.length, o + chunk)));
  const ms = performance.now() - t0;
  console.log(`p25 voice P1 C4FM (acquire+demod+FEC+IMBE), ${calls} call, ${(audioSamples / 8000).toFixed(1)} s audio out: ${fmt(ms, secs)}`);
}

// ── 3. P25 control channel: TSBK decode in 1 s windows ──────────────────────
{
  const blocks = [
    enc.buildIdenUp({ iden: 1, baseHz: 762_000_000, stepHz: 12_500, offsetHz: 30_000_000 }),
    enc.buildNetSts(0xbee00, 0x123, 80),
    enc.buildRfssSts(0x123, 1, 3, 80),
    enc.buildGrant(100, 101, 555, true),
  ];
  const one = enc.buildTsduDibits(0x4d8, blocks);
  const reps = Math.ceil((4800 * 10) / one.length);
  const dib = new Uint8Array(one.length * reps);
  for (let r = 0; r < reps; r++) dib.set(one, r * one.length);
  const iq = enc.dibitsToIq(dib, CH_FS, { carrierOffsetHz: 200, noise: 0.05 });
  const secs = iq.length / 2 / CH_FS;
  const win = 2 * CH_FS; // 1 s
  let msgs = 0;
  const t0 = performance.now();
  for (let o = 0; o + win <= iq.length; o += win) msgs += decodeP25OTA(iq.subarray(o, o + win), CH_FS).messages.length;
  console.log(`p25 control channel (TSBK decode), ${msgs} distinct msgs: ${fmt(performance.now() - t0, secs)}`);
}
