// The run's state as the page keeps it: the recorder's own, replaced with each
// `state` message (whether a restart waits is its verdict, not the page's). `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { defaultConfig } from "./config.ts";
import { holdsRadios, isRecording, snapshot, transport } from "./controller.ts";
import type { FromRecorder } from "./protocol.ts";

const state = (m: Partial<Extract<FromRecorder, { type: "state" }>>) => transport.onMessage({ type: "state", phase: "running", error: null, ended: false, restartNeeded: false, ...m });

test("the page's run state is the recorder's: a restart waits when it says, and an error lasts until its next state", () => {
  const c = defaultConfig();
  transport.onMessage({ type: "config", config: c, version: 1 });
  state({});
  // The config changes mid-run (a call sign heard, a plugin set): only the recorder says whether that waits for a restart.
  transport.onMessage({ type: "config", config: { ...c, recording: { ...c.recording, minCallS: 9 } }, version: 2 });
  assert.equal(snapshot().restartNeeded, false);
  state({ restartNeeded: true });
  assert.equal(snapshot().restartNeeded, true);
  // A refused Start's error, then another browser's Start: gone, as the recorder has it.
  state({ phase: "idle", error: "No source" });
  assert.equal(snapshot().runError, "No source");
  state({ phase: "starting" });
  assert.equal(snapshot().runError, null);
});

test("recording and holding the radios: Stopping is the one where they differ", () => {
  assert.deepEqual((["idle", "starting", "running", "stopping"] as const).map((p) => [isRecording(p), holdsRadios(p)]), [[false, false], [true, true], [true, true], [false, true]]);
});
