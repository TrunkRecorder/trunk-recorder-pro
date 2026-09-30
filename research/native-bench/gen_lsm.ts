// Synthetic SIMULCAST P25 system with ground truth — for measuring the
// simulcast (LSM/CQPSK) receiver: exact TSBK and IMBE codeword error rates,
// which real air can't give.
//
// One dongle's worth (2.4 MSPS u8): a CQPSK control channel that grants N
// CQPSK Phase 1 calls. Every signal goes through a two-transmitter simulcast
// channel — y = x + a·x(t−τ)·e^{j(2πΔf·t)} — then AWGN at Es/N0, then onto the
// wideband. Each voice frame is a random pitch (packImbe), so every LDU is
// distinct and decoded frames can be aligned to the truth.
//
//   node --experimental-strip-types gen_lsm.ts <prefix> [--dur 30] [--calls 2]
//        [--delay-us 40] [--echo 0.7] [--df 2] [--snr 20] [--seed 1]
// Writes <prefix>.cu8 and <prefix>.truth.json.

import { writeFileSync } from "node:fs";
import { buildGrant, buildIdenUp, buildNetSts, buildRfssSts, buildTsduDibits } from "../../archive/ts-engine/src/vendor/ff/p25/encode.ts";
import { dibitsToPi4Dqpsk } from "../../archive/ts-engine/src/vendor/ff/p25/cqpsk.ts";
import { p1CallDibits, packImbe } from "../../archive/ts-engine/src/vendor/ff/p25/voiceSim.ts";
import { imbeBitsToParams } from "../../archive/ts-engine/src/vendor/ff/p25/voice.ts";
import { SYS, IDEN, chanFor } from "../../archive/ts-engine/test/synth.ts";

const arg = (k: string, d: string) => {
  const i = process.argv.indexOf(`--${k}`);
  return i >= 0 ? Number(process.argv[i + 1]) : Number(d);
};
const prefix = process.argv[2];
const FS = 2_400_000;
const CH = 48_000;
const dur = arg("dur", "30");
const nCalls = arg("calls", "2");
const tau = arg("delay-us", "40") * 1e-6;
const echo = arg("echo", "0.7");
const df = arg("df", "2");
const snrDb = arg("snr", "20");
let seed = arg("seed", "1") >>> 0 || 1;
const rnd = () => ((seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0) / 4294967296);
const gauss = () => Math.sqrt(-2 * Math.log(rnd() + 1e-12)) * Math.cos(2 * Math.PI * rnd());

const CENTER = 773_100_000;
const CC = 773_000_000;
const calls = Array.from({ length: nCalls }, (_, k) => {
  const hz = CENTER - 600_000 + Math.round(((k + 1) * 1_200_000) / (nCalls + 1) / 12_500) * 12_500;
  const startS = 1 + 0.5 * k;
  const nFrames = Math.floor((dur - startS - 1.5) / 0.02 / 18) * 18;
  const frames = Array.from({ length: nFrames }, () => packImbe(20 + Math.floor(rnd() * 180)));
  return { hz, tg: 200 + k, src: 7000 + k, startS, frames };
});
const live = (t: number) => calls.filter((c) => t >= c.startS - 0.2 && t < c.startS + c.frames.length * 0.02 + 0.5);

// Control channel: TSDUs of 3 TSBKs, granting every live call; truth = each TSBK sent, with its time.
const ccDib: number[] = [];
const ccTruth: { t: number; hex: string }[] = [];
const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
for (let t = 0, k = 0; t < dur + 0.2; k++) {
  const l = live(t);
  const blocks = [
    k % 2 === 0 ? buildIdenUp(IDEN) : buildNetSts(SYS.wacn, SYS.sysId, chanFor(CC)),
    l.length ? buildGrant(chanFor(l[k % l.length].hz), l[k % l.length].tg, l[k % l.length].src) : buildRfssSts(SYS.sysId, SYS.rfss, SYS.site, chanFor(CC)),
    buildRfssSts(SYS.sysId, SYS.rfss, SYS.site, chanFor(CC), true),
  ];
  for (const b of blocks) ccTruth.push({ t: +t.toFixed(3), hex: hex(b) });
  const d = buildTsduDibits(SYS.nac, blocks);
  for (const x of d) ccDib.push(x);
  t += d.length / 4800;
}

// Simulcast channel at 48 kHz: direct path + delayed, frequency-offset echo.
function simulcast(iq: Float32Array): Float32Array {
  const n = iq.length / 2;
  const out = new Float32Array(iq.length);
  const d = tau * CH;
  const w = 2 * Math.PI * df;
  for (let i = 0; i < n; i++) {
    let er = 0;
    let ei = 0;
    const p = i - d;
    const i0 = Math.floor(p);
    if (i0 >= 0 && i0 + 1 < n) {
      const f = p - i0;
      er = iq[2 * i0] * (1 - f) + iq[2 * i0 + 2] * f;
      ei = iq[2 * i0 + 1] * (1 - f) + iq[2 * i0 + 3] * f;
    }
    const c = Math.cos((w * i) / CH) * echo;
    const s = Math.sin((w * i) / CH) * echo;
    out[2 * i] = iq[2 * i] + er * c - ei * s;
    out[2 * i + 1] = iq[2 * i + 1] + er * s + ei * c;
  }
  return out;
}

const n = Math.round(dur * FS);
const acc = new Float32Array(2 * n);
// Every signal at unit average power before the channel; Es/N0 → noise PSD.
const sig: { iq: Float32Array; hz: number; start: number }[] = [];
sig.push({ iq: simulcast(dibitsToPi4Dqpsk(Uint8Array.from(ccDib), CH, 4800)), hz: CC, start: 0 });
const truthCalls = calls.map((c) => {
  const dib = p1CallDibits({ nac: SYS.nac, tgid: c.tg, source: c.src, frames: c.frames });
  sig.push({ iq: simulcast(dibitsToPi4Dqpsk(dib, CH, 4800)), hz: c.hz, start: c.startS });
  return { hz: c.hz, tg: c.tg, startS: c.startS, u: c.frames.map((f) => imbeBitsToParams(f)) };
});
for (const s of sig) {
  const R = FS / CH;
  const w = (2 * Math.PI * (s.hz - CENTER)) / FS;
  const start = Math.round(s.start * FS);
  const m = s.iq.length / 2;
  for (let i = 0; i + start < n; i++) {
    const j = i / R;
    const j0 = Math.floor(j);
    if (j0 + 1 >= m) break;
    const f = j - j0;
    const a = s.iq[2 * j0] * (1 - f) + s.iq[2 * j0 + 2] * f;
    const b = s.iq[2 * j0 + 1] * (1 - f) + s.iq[2 * j0 + 3] * f;
    const c = Math.cos(w * (i + start));
    const sn = Math.sin(w * (i + start));
    acc[2 * (i + start)] += a * c - b * sn;
    acc[2 * (i + start) + 1] += a * sn + b * c;
  }
}
// AWGN: Es/N0 per channel with unit-power symbols → N0 = Tsym / 10^(snr/10);
// complex noise variance at FS = N0·FS.
const sigma = Math.sqrt((FS / 4800 / 10 ** (snrDb / 10)) / 2);
for (let i = 0; i < 2 * n; i++) acc[i] += sigma * gauss();
let pw = 0;
for (let i = 0; i < 2 * n; i++) pw += acc[i] * acc[i];
const scale = 0.3 / Math.sqrt(pw / (2 * n)); // rms 0.3 of full scale per component
const u8 = new Uint8Array(2 * n);
for (let i = 0; i < 2 * n; i++) u8[i] = Math.max(0, Math.min(255, Math.round(127.5 + 127.5 * acc[i] * scale)));
writeFileSync(`${prefix}.cu8`, u8);
writeFileSync(`${prefix}.truth.json`, JSON.stringify({ center: CENTER, cc: CC, dur, tau, echo, df, snrDb, ccTruth, calls: truthCalls }));
console.log(JSON.stringify({ center: CENTER, cc: CC, calls: truthCalls.map((c) => ({ hz: c.hz, tg: c.tg, frames: c.u.length })), tsbks: ccTruth.length }));
