// Synthetic P25 system for the native benchmark: a C4FM control channel that
// grants every live call, plus N overlapping Phase 1 calls spread across the
// band. Same construction as test/synth.ts, but each signal is scaled so N
// calls don't clip, and it writes both RTL-style u8 (.cu8) and float32 IQ
// (.cf32, what Trunk Recorder's iqfile source reads).
//
//   node --experimental-strip-types gen.ts <out-prefix> --fs 2400000 --dur 30 --calls 8

import { openSync, writeSync, closeSync } from "node:fs";
import { buildGrant, buildIdenUp, buildNetSts, buildRfssSts, buildTsduDibits, dibitsToC4fmShaped, dibitsToIq } from "../../archive/ts-engine/src/vendor/ff/p25/encode.ts";
import { imbeB0ForHz, melody, p1CallDibits, packImbe } from "../../archive/ts-engine/src/vendor/ff/p25/voiceSim.ts";
import { SYS, IDEN, chanFor } from "../../archive/ts-engine/test/synth.ts";

const arg = (k: string, d: string) => {
  const i = process.argv.indexOf(`--${k}`);
  return i >= 0 ? process.argv[i + 1] : d;
};
const prefix = process.argv[2];
const fs = Number(arg("fs", "2400000"));
const dur = Number(arg("dur", "30"));
const nCalls = Number(arg("calls", "8"));
// Multi-dongle: give the system's calls explicitly (--system hz,hz,…; the CC
// grants all of them) and render only this dongle's share (--render hz,…) at
// --center; --cc 0 leaves the control channel out of this capture.
const CENTER = Number(arg("center", "773100000"));
const CC = 773_000_000;
const renderCc = arg("cc", "1") === "1";
const list = (k: string) => arg(k, "").split(",").filter(Boolean).map(Number);

// Calls on the 12.5 kHz grid, spread over ±40 % of the band, skipping the CC.
const span = 0.8 * fs;
const calls: { hz: number; tg: number; src: number; startS: number; frames: number }[] = [];
const system = list("system");
for (let k = 0; k < system.length; k++) {
  const startS = 0.6 + 0.15 * k;
  calls.push({ hz: system[k], tg: 100 + k, src: 5000 + k, startS, frames: Math.floor((dur - startS - 0.8) / 0.02) });
}
for (let k = 0; k < (system.length ? 0 : nCalls); k++) {
  let off = -span / 2 + ((k + 0.5) * span) / nCalls;
  off = Math.round(off / 12_500) * 12_500;
  if (Math.abs(CENTER + off - CC) < 25_000) off += 50_000;
  const startS = 0.6 + 0.15 * k;
  calls.push({ hz: CENTER + off, tg: 100 + k, src: 5000 + k, startS, frames: Math.floor((dur - startS - 0.8) / 0.02) });
}
const render = system.length ? list("render") : calls.map((c) => c.hz);
const amp = 0.9 / (render.length + 1);

const n = Math.round(dur * fs);
const acc = new Float32Array(2 * n);
const live = (t: number) => calls.filter((c) => t >= c.startS - 0.1 && t < c.startS + c.frames * 0.02 + 0.6);

const ccDib: number[] = [];
const ccCh = chanFor(CC);
for (let t = 0, k = 0; t < dur + 0.2; k++) {
  const l = live(t);
  const c = l[k % Math.max(1, l.length)];
  const d = buildTsduDibits(SYS.nac, [
    k % 2 === 0 ? buildIdenUp(IDEN) : buildNetSts(SYS.wacn, SYS.sysId, ccCh),
    c ? buildGrant(chanFor(c.hz), c.tg, c.src) : buildRfssSts(SYS.sysId, SYS.rfss, SYS.site, ccCh),
    c ? buildGrant(chanFor(l[(k + 1) % l.length].hz), l[(k + 1) % l.length].tg, l[(k + 1) % l.length].src) : buildRfssSts(SYS.sysId, SYS.rfss, SYS.site, ccCh, true),
  ]);
  for (const x of d) ccDib.push(x);
  t += d.length / 4800;
}
const ccIq = dibitsToIq(Uint8Array.from(ccDib), fs, { carrierOffsetHz: CC - CENTER, amp });
if (renderCc) for (let i = 0; i < 2 * n; i++) acc[i] += ccIq[i];

for (const c of calls.filter((c) => render.includes(c.hz))) {
  const frames = melody(c.frames, [262, 330, 392, 523], 10, (hz: number) => packImbe(imbeB0ForHz(hz)));
  const v = dibitsToC4fmShaped(p1CallDibits({ nac: SYS.nac, tgid: c.tg, source: c.src, frames }), 48_000, {});
  const nv = v.length / 2;
  const R = fs / 48_000;
  const inst = new Float32Array(nv);
  for (let i = 1; i < nv; i++) {
    inst[i] = Math.atan2(v[2 * i + 1] * v[2 * i - 2] - v[2 * i] * v[2 * i - 1], v[2 * i] * v[2 * i - 2] + v[2 * i + 1] * v[2 * i - 1]) / R;
  }
  const start = Math.round(c.startS * fs);
  const w = (2 * Math.PI * (c.hz - CENTER)) / fs;
  let ph = 0;
  for (let i = 0; i < Math.floor(nv * R) && start + i < n; i++) {
    const j = i / R;
    const j0 = Math.floor(j);
    const fr = j - j0;
    ph += (inst[j0] ?? 0) * (1 - fr) + (inst[j0 + 1] ?? 0) * fr + w;
    acc[2 * (start + i)] += amp * Math.cos(ph);
    acc[2 * (start + i) + 1] += amp * Math.sin(ph);
  }
}

let seed = 12345;
const rnd = () => ((seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0) / 2 ** 32 - 0.5);
const u8 = new Uint8Array(2 * n);
for (let i = 0; i < 2 * n; i++) {
  u8[i] = Math.max(0, Math.min(255, Math.round(127.5 + 127.5 * (acc[i] + 0.02 * rnd()))));
  acc[i] = (u8[i] - 127.5) / 127.5; // the .cf32 is exactly the quantised u8, so every engine sees the same air
}
const w = (path: string, a: ArrayBufferView) => {
  const fd = openSync(path, "w");
  const CH = 64 << 20;
  const b = new Uint8Array(a.buffer, a.byteOffset, a.byteLength);
  for (let o = 0; o < b.length; o += CH) writeSync(fd, b, o, Math.min(CH, b.length - o));
  closeSync(fd);
};
w(`${prefix}.cu8`, u8);
w(`${prefix}.cf32`, acc);
console.log(JSON.stringify({ fs, dur, center: CENTER, cc: CC, calls: calls.map((c) => ({ hz: c.hz, tg: c.tg, startS: c.startS, s: +(c.frames * 0.02).toFixed(1) })) }));
