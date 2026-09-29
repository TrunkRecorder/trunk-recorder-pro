import { test } from "node:test";
import assert from "node:assert/strict";
import { CallManager, type Call } from "../src/trunking/callManager.ts";
import { parseTalkgroupCsv } from "../src/trunking/talkgroups.ts";
import { blankMessage, type TrunkMessage } from "../src/protocols/types.ts";

function grant(timeS: number, tg: number, hz: number, extra: Partial<TrunkMessage> = {}): TrunkMessage {
  return { ...blankMessage(timeS, 0), type: "grant", talkgroup: tg, freqHz: hz, source: 100, ...extra };
}

function setup(cfg = {}) {
  const log: string[] = [];
  const m = new CallManager(
    {
      startRecording: (c) => (c.freqHz > 900e6 ? "no_source" : (log.push(`start ${c.talkgroup}`), "ok")),
      stopRecording: (c) => void log.push(`stop ${c.talkgroup}`),
    },
    { onCallEnd: (c: Call) => void log.push(`end ${c.talkgroup}`) },
    cfg,
  );
  return { m, log };
}

test("grant starts a recording; repeats refresh it; both timeouts end it", () => {
  const { m, log } = setup();
  m.handle([grant(0, 101, 851e6)]);
  m.handle([grant(1, 101, 851e6), { ...grant(2, 101, 851e6), type: "update", source: -1 }]);
  assert.equal(m.calls.length, 1);
  m.noteAudio(m.calls[0], 4.5);
  m.tick(5.5); // 3.5 s since the last update, but audio 1 s ago → keep
  assert.equal(m.calls.length, 1);
  m.tick(7.6);
  assert.deepEqual(log, ["start 101", "stop 101", "end 101"]);
});

test("same talkgroup on another frequency is a separate call", () => {
  const { m } = setup();
  m.handle([grant(0, 101, 851e6), grant(0.1, 101, 852e6)]);
  assert.equal(m.calls.length, 2);
});

test("encrypted, out-of-band and unknown talkgroups are monitored with a reason", () => {
  const { m } = setup({ recordUnknown: false });
  m.talkgroups = parseTalkgroupCsv("Decimal,Alpha Tag,Mode\n101,Fire Disp,D\n202,Tac,E\n303,Far,D\n");
  m.handle([grant(0, 202, 851e6), grant(0, 303, 950e6), grant(0, 404, 851e6), grant(0, 101, 851e6, { encrypted: true })]);
  assert.deepEqual(
    m.calls.map((c) => [c.talkgroup, c.state, c.reason]),
    [
      [202, "monitoring", "encrypted"],
      [303, "monitoring", "no_source"],
      [404, "monitoring", "unknown_tg"],
      [101, "monitoring", "encrypted"],
    ],
  );
  m.tick(3.5);
  assert.equal(m.calls.length, 0, "monitoring calls end on the update timeout alone");
});

test("talkgroup CSV: headed and legacy formats", () => {
  const headed = parseTalkgroupCsv('Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category\n2501,9c5,"Fire, Main",D,Dispatch,Fire Dispatch,Fire\n');
  assert.deepEqual(headed.get(2501), { number: 2501, mode: "D", alphaTag: "Fire, Main", description: "Dispatch", tag: "Fire Dispatch", group: "Fire", priority: 1, preferredNac: 0 });
  const legacy = parseTalkgroupCsv("101,65,D,PD Disp,Police Dispatch,Law Dispatch,Police,2\n");
  assert.equal(legacy.get(101)?.alphaTag, "PD Disp");
  assert.equal(legacy.get(101)?.priority, 2);
});
