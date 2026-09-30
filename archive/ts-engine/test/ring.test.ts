import { test } from "node:test";
import assert from "node:assert/strict";
import { SampleRing } from "../src/engine/ring.ts";

test("ring round-trips across the wrap, in order", () => {
  const r = new SampleRing(10);
  const got: number[] = [];
  const dst = new Float32Array(4);
  let v = 0;
  for (let round = 0; round < 50; round++) {
    const src = Float32Array.from({ length: 3 }, () => v++);
    assert.ok(r.write(src));
    const n = r.read(dst);
    got.push(...dst.subarray(0, n));
  }
  assert.deepEqual(got, Array.from({ length: 150 }, (_, i) => i));
});

test("ring refuses (and counts) a write that would overflow; indices survive long runs", () => {
  const r = new SampleRing(8);
  assert.ok(r.write(new Float32Array(6)));
  assert.equal(r.write(new Float32Array(3)), false);
  assert.equal(r.stats().dropped, 3);
  assert.equal(r.size, 6);
  const dst = new Float32Array(8);
  // Many laps: the mirror-bit indices must keep full/empty unambiguous.
  for (let i = 0; i < 10_000; i++) {
    r.read(dst);
    assert.ok(r.write(new Float32Array(8).fill(i)));
    assert.equal(r.size, 8);
  }
  assert.equal(r.read(dst), 8);
  assert.equal(dst[7], 9999);
  assert.equal(r.size, 0);
});

test("a second view attached to the same buffer sees the data (worker hand-off)", () => {
  const a = new SampleRing(16);
  const b = new SampleRing(a.buffer);
  a.write(Float32Array.of(1, 2, 3));
  const dst = new Float32Array(3);
  assert.equal(b.read(dst), 3);
  assert.deepEqual([...dst], [1, 2, 3]);
});
