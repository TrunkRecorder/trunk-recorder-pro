// The browser build's page of calls: the call store's filter and paging
// (calls.ts). `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { callsPage } from "./calls.ts";
import type { CallEntry } from "./protocol.ts";

const call = (path: string, tg: number, start: number, units: number[] = []): CallEntry =>
  ({ path, audio: true, json: true, record: { short_name: "s", talkgroup: tg, start_time_ms: start, srcList: units.map((src) => ({ src })) } }) as unknown as CallEntry;

test("paging takes equal starts whole and loses nothing", () => {
  const calls = [call("a", 1, 5000), call("b", 2, 5000), call("c", 1, 4000, [77]), call("d", 2, 3000), call("e", 1, 1000)];
  const seen: string[] = [];
  let before: number | undefined;
  for (;;) {
    const p = callsPage(calls, { limit: 1, before });
    seen.push(...p.entries.map((c) => c.path));
    if (!p.more) break;
    before = p.before;
  }
  assert.deepEqual(seen, ["b", "a", "c", "d", "e"], "5000 came whole on the first page");
  assert.deepEqual(callsPage(calls, { talkgroup: 2 }).entries.map((c) => c.path), ["b", "d"]);
  assert.deepEqual(callsPage(calls, { unit: 77 }).entries.map((c) => c.path), ["c"]);
  assert.deepEqual(callsPage(calls, { from: 3000, to: 5000 }).entries.map((c) => c.path), ["c", "d"]);
  const none = callsPage(calls, { system: "other", before: 1234 });
  assert.deepEqual([none.entries.length, none.more, none.before], [0, false, 1234]);
});
