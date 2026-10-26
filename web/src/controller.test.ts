// An edit of the config is sent as editing the version it was made over, so
// one made over a config another browser has since changed is refused rather
// than saved over it; a system's talkgroup file follows its name only once the
// edit is saved, and not onto a name another system has. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { defaultConfig, newSystem } from "./config.ts";
import { renameSystem, setTalkgroups, snapshot, transport, updateConfig } from "./controller.ts";
import type { FromRecorder, ToRecorder } from "./protocol.ts";

/** A recorder with systems alpha and beta, each with a talkgroup file, its config at version 1. */
function connect(): void {
  const c = defaultConfig();
  c.systems.push(newSystem(c, { shortName: "alpha" }));
  c.systems.push(newSystem(c, { shortName: "beta" }));
  transport.onConnection(true);
  const greeting: FromRecorder[] = [
    { type: "config", config: c, version: 1 },
    { type: "talkgroups", system: "alpha", csv: "1,a" },
    { type: "talkgroups", system: "beta", csv: "2,b" },
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

test("a talkgroup file follows its system's name once the edit is saved, not onto another's name", async () => {
  connect();
  const onto = await sent(() => updateConfig((x) => renameSystem(x, x.systems[0], "beta")));
  assert.equal(onto.filter((m) => m.type === "setTalkgroups").length, 0, "nothing moved on the recorder");
  assert.deepEqual(snapshot().talkgroups, { alpha: "1,a", beta: "2,b" }, "beta's file is beta's; alpha's waits");
  transport.onMessage({ type: "configSaved", version: 2 });
  const refused = await sent(() => updateConfig((x) => renameSystem(x, x.systems[0], "gamma")));
  assert.equal(refused.filter((m) => m.type === "setTalkgroups").length, 0, "not before the edit is saved");
  assert.deepEqual(snapshot().talkgroups, { gamma: "1,a", beta: "2,b" }, "in the store it follows at once");
  transport.onMessage({ type: "configSaved", version: 2, error: "no" });
  assert.deepEqual(snapshot().talkgroups, { alpha: "1,a", beta: "2,b" }, "refused: back where the recorder has it");
  const saved = await sent(() => {
    updateConfig((x) => renameSystem(x, x.systems[0], "gamma"));
  });
  assert.equal(saved.filter((m) => m.type === "setTalkgroups").length, 0);
  const moves = await sent(() => transport.onMessage({ type: "configSaved", version: 3 }));
  assert.deepEqual(
    moves.filter((m) => m.type === "setTalkgroups"),
    [
      { type: "setTalkgroups", system: "gamma", csv: "1,a" },
      { type: "setTalkgroups", system: "alpha", csv: "" },
    ],
    "saved: the file moves on the recorder",
  );
});

const tgSends = (out: ToRecorder[]) => out.filter((m) => m.type === "setTalkgroups");

test("a talkgroup edit queued behind one in flight outlives a reconnect: the greeting's older file doesn't replace it, and it's sent again", async () => {
  connect();
  const out = await sent(() => {
    setTalkgroups("alpha", "1,a\n2,x");
    setTalkgroups("alpha", "1,a\n2,y");
    // The connection drops before either is acked; the greeting has the recorder's older file.
    transport.onConnection(true);
    transport.onMessage({ type: "talkgroups", system: "alpha", csv: "1,a" });
  });
  assert.equal(snapshot().talkgroups.alpha, "1,a\n2,y", "the greeting didn't undo the edit");
  assert.deepEqual(tgSends(out).at(-1), { type: "setTalkgroups", system: "alpha", csv: "1,a\n2,y" }, "the latest edit is the last sent");
  transport.onMessage({ type: "talkgroups", system: "alpha", csv: "1,a\n2,y" });
  transport.onMessage({ type: "talkgroups", system: "alpha", csv: "1,a\n2,z" });
  assert.equal(snapshot().talkgroups.alpha, "1,a\n2,z", "acked: another browser's edit is taken again");
});

test("another browser's talkgroup file arriving while an edit is in flight isn't taken as its ack", async () => {
  connect();
  const out = await sent(() => {
    setTalkgroups("beta", "2,b\n3,c");
    setTalkgroups("beta", "2,b\n3,d");
    transport.onMessage({ type: "talkgroups", system: "beta", csv: "2,b\n9,other" });
  });
  assert.equal(tgSends(out).length, 1, "the queued edit waits for the real echo");
  assert.equal(snapshot().talkgroups.beta, "2,b\n3,d");
  const next = await sent(() => transport.onMessage({ type: "talkgroups", system: "beta", csv: "2,b\n3,c" }));
  assert.deepEqual(tgSends(next), [{ type: "setTalkgroups", system: "beta", csv: "2,b\n3,d" }]);
  transport.onMessage({ type: "talkgroups", system: "beta", csv: "2,b\n3,d" });
  assert.equal(snapshot().talkgroups.beta, "2,b\n3,d");
});
