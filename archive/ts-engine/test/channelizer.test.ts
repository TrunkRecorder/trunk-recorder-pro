import { test } from "node:test";
import assert from "node:assert/strict";
import { Channelizer } from "../src/engine/channelizer.ts";

const FS = 2_400_000;

function tone(n: number, hz: number, from = 0): Float32Array {
  const iq = new Float32Array(2 * n);
  for (let i = 0; i < n; i++) {
    const p = (2 * Math.PI * hz * (i + from)) / FS;
    iq[2 * i] = Math.cos(p);
    iq[2 * i + 1] = Math.sin(p);
  }
  return iq;
}

function collect(c: Channelizer, off: number, pre = 0) {
  const out: number[] = [];
  c.addHead(off, 7000, (iq) => out.push(...iq), pre);
  return out;
}

test("picks a power-of-two decimation meeting the minimum rate", () => {
  const c = new Channelizer({ fs: FS, minOutputRate: 24_000 });
  assert.equal(c.decim, 64);
  assert.equal(c.outputRate, 37_500);
});

test("a tone lands at its residual offset at unit gain; a far channel rejects it", () => {
  const c = new Channelizer({ fs: FS, minOutputRate: 24_000 });
  const on = collect(c, 312_500);
  const off = collect(c, -600_000);
  c.push(tone(FS, 312_550));
  const tail = (a: number[]) => a.slice(a.length / 2);
  const a = tail(on);
  let p = 0;
  let dph = 0;
  for (let i = 0; i < a.length; i += 2) p += a[i] ** 2 + a[i + 1] ** 2;
  for (let i = 2; i < a.length; i += 2) dph += Math.atan2(a[i + 1] * a[i - 2] - a[i] * a[i - 1], a[i] * a[i - 2] + a[i + 1] * a[i - 1]);
  const hz = ((dph / (a.length / 2 - 1)) * c.outputRate) / (2 * Math.PI);
  assert.ok(Math.abs(10 * Math.log10(p / (a.length / 2))) < 0.05, "unit gain");
  assert.ok(Math.abs(hz - 50) < 0.5, `residual ${hz} Hz`);
  const b = tail(off);
  let q = 0;
  for (let i = 0; i < b.length; i += 2) q += b[i] ** 2 + b[i + 1] ** 2;
  assert.ok(10 * Math.log10(q / (b.length / 2)) < -100, "rejection");
});

test("a head added late with pre-roll reproduces what a head present all along produced", () => {
  const whole = new Channelizer({ fs: FS, minOutputRate: 24_000, historyS: 1 });
  const late = new Channelizer({ fs: FS, minOutputRate: 24_000, historyS: 1 });
  const ref = collect(whole, 250_037);
  const sig = tone(FS * 2, 250_037 + 1234);
  const half = sig.length / 2;
  whole.push(sig);
  late.push(sig.subarray(0, half));
  const blocksBefore = Math.floor(half / 2 / late.l);
  const out = collect(late, 250_037, 0.5);
  late.push(sig.subarray(half));
  // `late` replayed ceil(0.5 s / block) blocks; its first sample is that block of `ref`.
  const replayed = Math.ceil((0.5 * FS) / late.l);
  const offset = (blocksBefore - replayed) * (late.l / late.decim) * 2;
  let maxErr = 0;
  for (let i = 0; i < out.length && offset + i < ref.length; i++) maxErr = Math.max(maxErr, Math.abs(out[i] - ref[offset + i]));
  assert.ok(out.length > 0 && maxErr < 1e-5, `max error ${maxErr}`);
});
