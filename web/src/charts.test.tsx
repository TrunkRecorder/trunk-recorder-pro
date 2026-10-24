// The time series chart: its scales.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { TimeSeries, type Line } from "./charts.tsx";

const series = (v: number[], hi = v) => ({ t0: 0, stepS: 60, v, lo: v.map(() => 0), hi, n: v.map(() => 1) });
const labels = (html: string, anchor: "end" | "start") => [...html.matchAll(new RegExp(`text-anchor="${anchor}" class="axis"[^>]*>([^<]+)<`, "g"))].map((m) => m[1]).filter((s) => /^\d+$/.test(s));

test("a line in other units gets its own scale on the right; an undrawn band doesn't stretch a scale", () => {
  const calls: Line = { label: "Calls /min", color: "red", data: series([2, 3, 4]) };
  // A minute's spike (5000) inside the steps: their highest, not drawn.
  const air: Line = { label: "On the air, s/min", color: "blue", data: series([100, 200, 300], [100, 5000, 300]), right: true };
  const html = renderToString(<TimeSeries lines={[calls, air]} zero fmt={(v) => v.toFixed(0)} />);
  const left = labels(html, "end").map(Number);
  const right = labels(html, "start").map(Number);
  assert.ok(Math.max(...left) <= 5, `left ${left}`);
  assert.ok(Math.max(...right) >= 300 && Math.max(...right) < 1000, `right ${right}`);
  // One scale when nothing's on the right.
  assert.deepEqual(labels(renderToString(<TimeSeries lines={[calls]} zero fmt={(v) => v.toFixed(0)} />), "start"), []);
});
