// The control channel log keeps each system's lines apart (controller.ts), so a
// busy system's don't push a quiet one's out. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { snapshot, transport, withLog } from "./controller.ts";
import type { LogLine } from "./protocol.ts";

const line = (system: string | undefined, n: number): LogLine => ({ timeS: n, kind: "grant", text: `${system} ${n}`, system });

test("withLog: a busy system doesn't push out a quiet one's lines", () => {
  let log: Record<string, LogLine[]> = withLog({}, [line("quiet", 1), line("quiet", 2)]);
  for (let i = 0; i < 100; i++) log = withLog(log, Array.from({ length: 50 }, (_, j) => line("busy", i * 50 + j)), 500);
  assert.deepEqual(log.quiet.map((l) => l.timeS), [1, 2]);
  assert.equal(log.busy.length, 500);
  assert.deepEqual([log.busy[0].timeS, log.busy[499].timeS], [4500, 4999], "the newest kept, in order");
});

test("withLog: the recorder's own lines (no system's) aren't kept; a batch is split by system", () => {
  const log = withLog({ a: [line("a", 1)] }, [line("b", 2), line(undefined, 3), line("a", 4)]);
  assert.deepEqual(Object.fromEntries(Object.entries(log).map(([k, v]) => [k, v.map((l) => l.timeS)])), { a: [1, 4], b: [2] });
});

test("A log message fills each system's own log", () => {
  transport.onMessage({ type: "log", lines: [line("dcfd", 1), line("wmata", 2), { timeS: 0, time: 1, level: "warning", kind: "error", text: "Source 1: no samples for 2 s" }] });
  const log = snapshot().log;
  assert.deepEqual([log.dcfd?.map((l) => l.text), log.wmata?.map((l) => l.text)], [["dcfd 1"], ["wmata 2"]]);
});
