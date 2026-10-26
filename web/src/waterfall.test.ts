import { test } from "node:test";
import assert from "node:assert/strict";
import { readout } from "./Waterfall.tsx";

// 101 bins across 2.4 MHz at 154.5 MHz: 24 kHz a bin, bin 50 the centre.
const flat = () => Array.from({ length: 101 }, () => -100);

test("Waterfall readout: the frequency under the pointer, on the noise", () => {
  const r = readout(flat(), 154_500_000, 2_400_000, -100, 0.25);
  assert.equal(r.hz, 153_900_000);
  assert.equal(r.overDb, null);
  assert.equal(r.x, 0.25);
});

test("Waterfall readout: snapped to a signal's peak near the pointer, with how strong it is", () => {
  const s = flat();
  s[60] = -70;
  s[61] = -80;
  const r = readout(s, 154_500_000, 2_400_000, -100, 0.57);
  assert.equal(r.hz, 154_740_000);
  assert.equal(r.overDb, 30);
  assert.equal(r.x, 0.6);
  // Too far from the pointer, or too weak: not snapped to.
  assert.equal(readout(s, 154_500_000, 2_400_000, -100, 0.3).overDb, null);
  s[60] = s[61] = -95;
  assert.equal(readout(s, 154_500_000, 2_400_000, -100, 0.57).overDb, null);
});
