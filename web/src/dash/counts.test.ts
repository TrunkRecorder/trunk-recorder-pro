// Reset counts: the since-start totals count from the reset, in this browser;
// a new run (Start) counts from nothing, so all of it is since the reset; the
// RAM spool's counts outlast a run. `npm test`.
import "../testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { clearCountsReset, resetCounts, snapshot, transport } from "../controller.ts";
import type { FromRecorder } from "../protocol.ts";
import { callsCounted, diskCounted, pluginCounted, sinceLabel, sourceCounts } from "./counts.ts";

/** The recorder's status: one radio and one system, with these totals. */
function status(dropped: number, errors: number, calls: number): void {
  const system = { index: 0, shortName: "metro", activeCalls: 0, recording: 0, conventionalOpen: 0, callsConcluded: calls, sites: [] };
  const m = {
    type: "status",
    status: { nowS: 1, activeCalls: 0, recording: 0, channelsOpen: 0, conventionalOpen: 0, callsConcluded: calls, systems: [system], systemNames: ["metro"] },
    sources: [{ index: 0, label: "rtl0", centerHz: 0, rateHz: 0, rateMeasured: 0, dropped, errors, lastError: null, ended: false }],
    load: 0,
    calls: [],
  };
  transport.onMessage(m as unknown as FromRecorder);
}

function host(overflowed: number): void {
  const m = { type: "host", t: 1, values: {}, platform: { disks: [{ name: "spool", path: "", mount: "", totalBytes: 1, freeBytes: 1, overflowed, removed: 0 }] } };
  transport.onMessage(m as unknown as FromRecorder);
}

function plugin(ok: number, failed: number, reasons: { outcome: "skipped" | "failed"; reason: string; calls: number }[]): void {
  const runtime = { state: "ok", message: "", ok, skipped: 0, failed, lastFailure: "", log: [], restarts: 1, dropped: 0, reasons };
  transport.onMessage({ type: "plugins", plugins: [{ id: "up", path: "", runtime }] } as unknown as FromRecorder);
}

const counts = () => {
  const s = snapshot();
  const p = s.plugins?.plugins[0] ? pluginCounted(s, s.plugins.plugins[0]).runtime : null;
  const spool = s.host ? diskCounted(s, s.host.platform.disks[0]) : null;
  return { ...sourceCounts(s, s.sources[0]), calls: callsCounted(s), metro: callsCounted(s, s.status!.systems[0]), ok: p?.ok, failed: p?.failed, restarts: p?.restarts, reasons: p?.reasons, overflowed: spool?.overflowed };
};

test("the totals count from the reset, and from start again once cleared", () => {
  transport.onConnection(true);
  status(5, 1, 40);
  plugin(30, 2, [{ outcome: "failed", reason: "timeout", calls: 2 }]);
  host(4);
  assert.equal(sinceLabel(snapshot()), "Since start");
  assert.deepEqual(counts(), { dropped: 5, errors: 1, calls: 40, metro: 40, ok: 30, failed: 2, restarts: 1, reasons: [{ outcome: "failed", reason: "timeout", calls: 2 }], overflowed: 4 });

  resetCounts();
  assert.match(sinceLabel(snapshot()), /^Since \d/);
  assert.deepEqual(counts(), { dropped: 0, errors: 0, calls: 0, metro: 0, ok: 0, failed: 0, restarts: 0, reasons: [], overflowed: 0 });

  status(12, 1, 43);
  plugin(35, 3, [{ outcome: "failed", reason: "timeout", calls: 2 }, { outcome: "failed", reason: "refused", calls: 1 }]);
  host(6);
  assert.deepEqual(counts(), { dropped: 7, errors: 0, calls: 3, metro: 3, ok: 5, failed: 1, restarts: 0, reasons: [{ outcome: "failed", reason: "refused", calls: 1 }], overflowed: 2 });

  // Start again: the run's totals count from nothing (all since the reset); the spool's go on.
  transport.onMessage({ type: "state", phase: "starting", error: null, ended: false, restartNeeded: false });
  status(20, 0, 1);
  plugin(4, 0, []);
  assert.deepEqual(counts(), { dropped: 20, errors: 0, calls: 1, metro: 1, ok: 4, failed: 0, restarts: 1, reasons: [], overflowed: 2 });

  // A total lower than at the reset counted again from nothing, unseen: all of it is since.
  host(1);
  assert.equal(counts().overflowed, 1);

  clearCountsReset();
  assert.equal(sinceLabel(snapshot()), "Since start");
  status(9, 0, 2);
  assert.equal(counts().dropped, 9);
});
