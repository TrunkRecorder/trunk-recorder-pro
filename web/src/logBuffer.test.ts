// The control channel log keeps each system's lines apart (controller.ts), so a
// busy system's don't push a quiet one's out. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { keepLog } from "./controller.ts";
import type { LogLine } from "./protocol.ts";

const line = (system: string | undefined, n: number): LogLine => ({ timeS: n, kind: "grant", text: `${system} ${n}`, system });

test("keepLog: a busy system doesn't push out a quiet one's lines", () => {
  let log: LogLine[] = [line("quiet", 1), line("quiet", 2)];
  for (let i = 0; i < 100; i++) log = keepLog(log, Array.from({ length: 50 }, (_, j) => line("busy", i * 50 + j)), 500);
  assert.deepEqual(log.filter((l) => l.system === "quiet").map((l) => l.timeS), [1, 2]);
  const busy = log.filter((l) => l.system === "busy");
  assert.equal(busy.length, 500);
  assert.equal(busy[499].timeS, 4999, "the newest kept");
  assert.equal(busy[0].timeS, 4500);
});

test("keepLog: order is kept across systems, and lines with no system have their own share", () => {
  const log = keepLog([line("a", 1), line(undefined, 2)], [line("b", 3), line("a", 4), line(undefined, 5)], 1);
  assert.deepEqual(log.map((l) => l.timeS), [3, 4, 5]);
});
