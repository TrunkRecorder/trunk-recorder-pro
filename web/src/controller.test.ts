// An edit of the config is sent as editing the version it was made over, so
// one made over a config another browser has since changed is refused rather
// than saved over it; a system's talkgroups follow its name only once the
// edit is saved, and not onto a name another system has. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { defaultConfig, newSystem } from "./config.ts";
import { renameSystem, setTalkgroupIgnore, snapshot, transport, updateConfig } from "./controller.ts";
import { talkgroup } from "./talkgroups.ts";
import type { FromRecorder, ToRecorder } from "./protocol.ts";

const [A, B] = [[talkgroup(1, { alphaTag: "a" })], [talkgroup(2, { alphaTag: "b" })]];

/** A recorder with systems alpha and beta, each with talkgroups, its config at version 1. */
function connect(): void {
  const c = defaultConfig();
  c.systems.push(newSystem(c, { shortName: "alpha" }));
  c.systems.push(newSystem(c, { shortName: "beta" }));
  transport.onConnection(true);
  const greeting: FromRecorder[] = [
    { type: "config", config: c, version: 1 },
    { type: "talkgroups", system: "alpha", talkgroups: A },
    { type: "talkgroups", system: "beta", talkgroups: B },
    { type: "hello", version: "test", platform: "test", api: 1, surveyBands: [], capabilities: [] },
  ];
  for (const m of greeting) transport.onMessage(m);
}

/** Sent to the recorder while `f` runs (and its edits settle). */
async function sent(f: () => void): Promise<ToRecorder[]> {
  const out: ToRecorder[] = [];
  const send = transport.send.bind(transport);
  transport.send = (m) => void out.push(m);
  try {
    f();
    await new Promise((r) => setTimeout(r, 400));
  } finally {
    transport.send = send;
  }
  return out;
}

test("an edit is sent as editing the version it was made over, and a refused one is taken back", async () => {
  connect();
  const other = structuredClone(snapshot().config!);
  other.systems[1].name = "Beta County";
  const edits = await sent(() => {
    updateConfig((x) => void (x.systems[0].name = "Alpha County"));
    // Another browser saved meanwhile: this one's edit was made over version 1.
    transport.onMessage({ type: "config", config: other, version: 2 });
  });
  const edit = edits.find((m) => m.type === "setConfig");
  assert.ok(edit && edit.type === "setConfig");
  assert.equal(edit.version, 1, "the version the edit was made over, not the newest heard");
  assert.equal(snapshot().config!.systems[0].name, "Alpha County", "the edit is kept while it's being saved");
  transport.onMessage({ type: "configSaved", version: 2, error: "the config changed meanwhile" });
  assert.equal(snapshot().config!.systems[0].name, undefined, "refused: the recorder's config again");
  assert.equal(snapshot().config!.systems[1].name, "Beta County");
  const again = await sent(() => updateConfig((x) => void (x.systems[0].name = "Alpha")));
  const next = again.find((m) => m.type === "setConfig");
  assert.ok(next && next.type === "setConfig");
  assert.equal(next.version, 2, "the next edit is over the config the refusal gave");
  transport.onMessage({ type: "configSaved", version: 3 });
});

test("talkgroups follow their system's name once the edit is saved, not onto another's name", async () => {
  connect();
  const onto = await sent(() => updateConfig((x) => renameSystem(x, x.systems[0], "beta")));
  assert.equal(onto.filter((m) => m.type === "setTalkgroups").length, 0, "nothing moved on the recorder");
  assert.deepEqual(snapshot().talkgroups, { alpha: A, beta: B }, "beta's are beta's; alpha's wait");
  transport.onMessage({ type: "configSaved", version: 2 });
  const refused = await sent(() => updateConfig((x) => renameSystem(x, x.systems[0], "gamma")));
  assert.equal(refused.filter((m) => m.type === "setTalkgroups").length, 0, "not before the edit is saved");
  assert.deepEqual(snapshot().talkgroups, { gamma: A, beta: B }, "in the store it follows at once");
  transport.onMessage({ type: "configSaved", version: 2, error: "no" });
  assert.deepEqual(snapshot().talkgroups, { alpha: A, beta: B }, "refused: back where the recorder has it");
  const saved = await sent(() => {
    updateConfig((x) => renameSystem(x, x.systems[0], "gamma"));
  });
  assert.equal(saved.filter((m) => m.type === "setTalkgroups").length, 0);
  const moves = await sent(() => transport.onMessage({ type: "configSaved", version: 3 }));
  assert.deepEqual(
    moves.filter((m) => m.type === "setTalkgroups"),
    [
      { type: "setTalkgroups", system: "gamma", talkgroups: A },
      { type: "setTalkgroups", system: "alpha", talkgroups: [] },
    ],
    "saved: they move on the recorder",
  );
});

/** Whether talkgroup `tg` of `system` is ignored, as this browser shows it. */
const ignored = (system: string, tg: number) => snapshot().talkgroups[system]?.find((t) => t.number === tg)?.ignore ?? false;

test("Ignore clicked quickly: each click shown at once, sent as its row alone, and the last one stands", async () => {
  connect();
  const out = await sent(() => {
    setTalkgroupIgnore("alpha", 1, true);
    setTalkgroupIgnore("alpha", 1, false);
    setTalkgroupIgnore("alpha", 1, true);
  });
  assert.deepEqual(out.map((m) => m.type), ["setTalkgroup", "setTalkgroup", "setTalkgroup"], "no whole lists");
  assert.ok(ignored("alpha", 1));
  assert.equal(snapshot().saveStatus, "saving");
  // Another browser's edit, made by the recorder before these: made here too, these over it.
  transport.onMessage({ type: "talkgroupEdited", system: "alpha", talkgroup: 1, edit: { alphaTag: "Dispatch" } });
  assert.ok(ignored("alpha", 1), "still as last clicked");
  assert.equal(snapshot().talkgroups.alpha.find((t) => t.number === 1)?.alphaTag, "Dispatch");
  // The answers, in order: the state never flips back meanwhile.
  for (const ignore of [true, false, true]) {
    transport.onMessage({ type: "talkgroupEdited", system: "alpha", talkgroup: 1, edit: { ignore } });
    assert.ok(ignored("alpha", 1));
  }
  assert.equal(snapshot().saveStatus, "saved");
});

test("An Ignore not answered before the connection dropped is sent again, and shown meanwhile", async () => {
  connect();
  await sent(() => setTalkgroupIgnore("beta", 2, true));
  const again = await sent(connect);
  assert.ok(again.some((m) => m.type === "setTalkgroup" && m.system === "beta" && m.talkgroup === 2 && m.edit.ignore === true));
  assert.ok(ignored("beta", 2), "over the greeting's talkgroups");
  transport.onMessage({ type: "talkgroupEdited", system: "beta", talkgroup: 2, edit: { ignore: true } });
  assert.equal(snapshot().saveStatus, "saved");
});

test("Setup's save status: saving until the recorder answers, then saved, or why not", async () => {
  connect();
  await sent(() => updateConfig((x) => void (x.systems[0].name = "Alpha County")));
  assert.equal(snapshot().saveStatus, "saving");
  transport.onMessage({ type: "configSaved", version: 2 });
  assert.equal(snapshot().saveStatus, "saved");
  await sent(() => updateConfig((x) => void (x.systems[0].name = "Alpha")));
  transport.onMessage({ type: "configSaved", version: 2, error: "The config was changed elsewhere meanwhile" });
  assert.deepEqual([snapshot().saveStatus, snapshot().saveError], ["failed", "The config was changed elsewhere meanwhile"]);
  // A file the recorder couldn't write in the background says so too.
  await sent(() => updateConfig((x) => void (x.systems[0].name = "Alpha 2")));
  transport.onMessage({ type: "configSaved", version: 3 });
  transport.onMessage({ type: "error", message: "Couldn't save /data/alpha.talkgroups.csv: disk full" });
  assert.equal(snapshot().saveStatus, "failed");
});
