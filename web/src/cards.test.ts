// Setup's card keys: each card stays its entry's as others come and go,
// and the keys never reach the recorder. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { defaultConfig, newConventional, newSystem } from "./config.ts";
import { cardKey, snapshot, transport, updateConfig } from "./controller.ts";
import type { FromRecorder, ToRecorder } from "./protocol.ts";

test("a card key follows its system when one before it is removed, and isn't sent", async () => {
  const c = defaultConfig();
  c.systems.push(newSystem(c, { shortName: "alpha" }));
  c.systems.push(newSystem(c, { shortName: "beta" }));
  transport.onConnection(true);
  const greeting: FromRecorder[] = [
    { type: "config", config: c, version: 1 },
    { type: "hello", version: "test", platform: "test", api: 1, surveyBands: [], capabilities: [] },
  ];
  for (const m of greeting) transport.onMessage(m);
  const key = (i: number) => cardKey(snapshot().config!.systems[i], `sys:${snapshot().config!.systems[i].shortName}`, snapshot().config!.systems);
  const beta = key(1);
  const sent: ToRecorder[] = [];
  const send = transport.send.bind(transport);
  transport.send = (m) => void sent.push(m);
  try {
    updateConfig((x) => void x.systems.splice(0, 1));
    assert.equal(snapshot().config!.systems[0].shortName, "beta");
    assert.equal(key(0), beta, "beta's card is still beta's");
    await new Promise((r) => setTimeout(r, 400));
    const save = sent.find((m) => m.type === "setConfig");
    assert.ok(save, "the edit was sent");
    assert.ok(!JSON.stringify(save).includes("_card"), "without card keys");
  } finally {
    transport.send = send;
  }
});

test("a card keeps its key when the recorder's copy of a rename comes back after the save is answered", async () => {
  const c = defaultConfig();
  c.conventional.push(newConventional(c, { shortName: "fire", channels: [{ freqHz: 154430000, mode: "fm", name: "", enabled: true }] }));
  transport.onConnection(true);
  transport.onMessage({ type: "config", config: c, version: 1 });
  const conv = () => snapshot().config!.conventional;
  const key = () => cardKey(conv()[conv().length - 1], `conv:${conv()[conv().length - 1].shortName}`, conv());
  const before = key();
  const sent: ToRecorder[] = [];
  const send = transport.send.bind(transport);
  transport.send = (m) => void sent.push(m);
  try {
    updateConfig((x) => void (x.conventional[x.conventional.length - 1].shortName = "fire2"));
    await new Promise((r) => setTimeout(r, 400));
    const save = sent.find((m) => m.type === "setConfig");
    assert.ok(save && save.type === "setConfig");
    // The answer, then the recorder's copy (no card keys), as a pause in typing brings them.
    transport.onMessage({ type: "configSaved", version: 2 });
    transport.onMessage({ type: "config", config: save.config, version: 2 });
    assert.equal(conv()[conv().length - 1].shortName, "fire2");
    assert.equal(key(), before, "the card is still its own: it stays open");
  } finally {
    transport.send = send;
  }
});
