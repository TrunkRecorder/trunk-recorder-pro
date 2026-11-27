import { test } from "node:test";
import assert from "node:assert/strict";
import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { labelPx, nearestMark, readout, stackLabels, Waterfall, type CcMark } from "./Waterfall.tsx";
import type { CallView } from "./protocol.ts";

test("Waterfall labels: apart, each on the top row; two that would overlap, the second a row down", () => {
  // 800 px: 10 % is 80 px. A 60 px label at 10 % runs to 31 % or so.
  assert.deepEqual(stackLabels([{ at: 10, px: 60 }, { at: 50, px: 60 }], 800, 2), [{ row: 0, left: false }, { row: 0, left: false }]);
  // "lcra/Seguin" and "New Braunfels" a few kHz apart: the second stacks, never printed over the first.
  const seguin = labelPx("lcra/Seguin");
  const nb = labelPx("lcra/New Braunfels");
  assert.deepEqual(stackLabels([{ at: 40, px: seguin }, { at: 43, px: nb }], 800, 2).map((p) => p.row), [0, 1]);
  // Whatever order they come in: the leftmost has the top row.
  assert.deepEqual(stackLabels([{ at: 43, px: nb }, { at: 40, px: seguin }], 800, 2).map((p) => p.row), [1, 0]);
  // A third clear of the first goes back up to the top row.
  assert.deepEqual(stackLabels([{ at: 10, px: 60 }, { at: 12, px: 60 }, { at: 30, px: 60 }], 800, 2).map((p) => p.row), [0, 1, 0]);
});

test("Waterfall labels: right of three quarters, drawn left of the line; more than the rows hold, the row that frees first", () => {
  const p = stackLabels([{ at: 90, px: 100 }], 800, 2);
  assert.deepEqual(p, [{ row: 0, left: true }]);
  // Flipped, a label right of another's line no longer reaches it: 70 % runs right to ~79 %, 90 % flipped starts at ~77 %.
  assert.deepEqual(stackLabels([{ at: 70, px: 60 }, { at: 90, px: 100 }], 800, 2).map((x) => x.row), [0, 1]);
  assert.deepEqual(stackLabels([{ at: 70, px: 60 }, { at: 95, px: 100 }], 800, 2).map((x) => x.row), [0, 0]);
  // Three on one spot, two rows: the third shares the row that ends first.
  const three = stackLabels([{ at: 10, px: 40 }, { at: 10.5, px: 120 }, { at: 11, px: 60 }], 800, 2);
  assert.deepEqual(three.map((x) => x.row), [0, 1, 0]);
  assert.deepEqual(stackLabels([], 800, 2), []);
});

test("Waterfall hover: the line nearest the pointer, within a few pixels; none further", () => {
  assert.equal(nearestMark([10, 50, 52], 0.505, 800), 1);
  assert.equal(nearestMark([10, 50, 52], 0.515, 800), 2);
  assert.equal(nearestMark([10, 50], 0.3, 800), -1);
  assert.equal(nearestMark([], 0.5, 800), -1);
});

test("Waterfall: site names on the trace; an adjacent site's address only on hover; calls tagged on their own row, fading; the source short, its label on hover", () => {
  const radio = { source: 0, centerHz: 852_000_000, rateHz: 4_000_000, bins: Array.from({ length: 64 }, () => -100) };
  const ccs: CcMark[] = [
    { hz: 851_000_000, label: "lcra/Seguin", color: "#4a8", title: "lcra/Seguin control channel" },
    { hz: 851_012_500, label: "lcra/New Braunfels", color: "#4a8", title: "lcra/New Braunfels control channel" },
    { hz: 853_000_000, label: "BEE00:954:1-1", color: "#4a8", faint: true, title: "Adjacent site BEE00:954:1-1 (inter-system), announced by lcra/Seguin" },
  ];
  const calls: Partial<CallView>[] = [{ id: 7, systemName: "lcra", talkgroup: 101, alphaTag: "Fire Disp", freqHz: 852_500_000, state: "recording", encrypted: false }];
  const html = renderToString(createElement(Waterfall, { radio, label: "SoapySDR · 700-band", labelTitle: "SoapySDR driver=remote,remote=tcp://radio.example:55132", ccs, calls: calls as CallView[] }));
  const shown = html.replace(/<[^>]+>/g, " ");
  assert.match(shown, /lcra\/Seguin/);
  assert.match(shown, /lcra\/New Braunfels/);
  assert.doesNotMatch(shown, /BEE00:954:1-1/, "the address isn't printed on the trace");
  assert.match(html, /class="mk mk-cc faint"[^>]*title="Adjacent site BEE00:954:1-1/, "its line says it on hover");
  // The two close site names: one on the top row, the other a row down.
  assert.match(html, /top:4px[^"]*">lcra\/Seguin</);
  assert.match(html, /top:22px[^"]*">lcra\/New Braunfels</);
  // The call: its own row, below the sites', in the class that fades it.
  assert.match(html, /class="mk mk-call mk-recording"[^>]*><i style="top:44px">Fire Disp</);
  assert.match(html, /<span class="clip" title="SoapySDR driver=remote,remote=tcp:\/\/radio.example:55132">SoapySDR · 700-band<\/span>/);
});

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
