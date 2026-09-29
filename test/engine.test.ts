// End to end on a synthetic system: wideband u8 → Channelizer → TrunkEngine →
// concluded calls. The same wiring as tools/replay.ts and the browser workers.

import { test } from "node:test";
import assert from "node:assert/strict";
import { Channelizer } from "../src/engine/channelizer.ts";
import { driverFor } from "../src/protocols/registry.ts";
import { TrunkEngine, type ChannelPort, type ConcludedCall } from "../src/trunking/trunkEngine.ts";
import type { TrunkMessage } from "../src/protocols/types.ts";
import type { Call } from "../src/trunking/callManager.ts";
import { SYS, synthSystem } from "./synth.ts";

const FS = 2_400_000;
const CENTER = 773_100_000;
const CC = 773_000_000;

function run(u8: Uint8Array, opts: { prerollS?: number; keepSilent?: boolean } = {}) {
  const driver = driverFor("p25");
  const chz = new Channelizer({ fs: FS, minOutputRate: driver.minChannelRate, historyS: 1 });
  const port: ChannelPort = {
    open: (off, cut, pre, onIq) => chz.addHead(off, cut, onIq, pre).id,
    close: (id) => chz.removeHead(id),
  };
  const msgs: TrunkMessage[] = [];
  const started: Call[] = [];
  const done: ConcludedCall[] = [];
  const engine = new TrunkEngine(
    {
      system: { shortName: "test", type: "p25", controlChannels: [CC] },
      centerHz: CENTER,
      rateHz: FS,
      channelRate: chz.outputRate,
      prerollS: opts.prerollS ?? 1,
      maxRecorders: 8,
      keepSilentCalls: opts.keepSilent ?? false,
      calls: {},
      epochMsAtZero: 1_700_000_000_000,
    },
    driver,
    port,
    { onMessages: (m) => msgs.push(...m), onCallStart: (c) => started.push(c), onConcluded: (c) => done.push(c) },
  );
  engine.start();
  for (let o = 0; o < u8.length; o += 131072) chz.pushU8(u8.subarray(o, Math.min(u8.length, o + 131072)));
  const status = engine.status();
  return { msgs, started, done, status, engine };
}

test("a granted P25 call is followed, recorded and concluded", () => {
  const u8 = synthSystem({
    fs: FS,
    centerHz: CENTER,
    ccHz: CC,
    durationS: 9,
    calls: [{ hz: 773_250_000, tg: 101, src: 555, startS: 1.0, frames: 150 }],
  });
  const { msgs, started, done, status } = run(u8);

  assert.equal(status.identity.nac, SYS.nac);
  assert.equal(status.identity.wacn, SYS.wacn);
  assert.equal(status.identity.sysId, SYS.sysId);
  assert.equal(status.modulation, "C4FM");
  assert.ok(msgs.some((m) => m.type === "grant" && m.talkgroup === 101 && m.freqHz === 773_250_000 && m.source === 555));

  assert.equal(started.length, 1);
  assert.equal(started[0].state, "recording");
  assert.equal(done.length, 1, "call concluded after the grants stopped");
  const c = done[0];
  assert.equal(c.record.talkgroup, 101);
  assert.equal(c.record.freq, 773_250_000);
  assert.equal(c.record.encrypted, 0);
  assert.deepEqual(c.record.srcList.map((s) => s.src), [555]);
  // 150 frames → padded to 162 (9 LDU pairs) = 3.24 s of voice. Pre-roll puts the
  // HDU in view, so none of it is lost to the grant's latency.
  const secs = c.audio.length / 8000;
  assert.ok(secs > 3.0 && secs <= 3.3, `audio ${secs} s`);
  assert.equal(c.baseName, `101-${c.record.start_time}_773250000`);
});

test("TSBKs are delivered once each, in order (no window double-counting)", () => {
  const u8 = synthSystem({ fs: FS, centerHz: CENTER, ccHz: CC, durationS: 4, calls: [] });
  const { msgs, status } = run(u8);
  // Every TSDU carries 3 blocks; the channel runs ~4 s at ~12.3 TSDUs/s.
  const times = msgs.map((m) => m.timeS);
  for (let i = 1; i < times.length; i++) assert.ok(times[i] >= times[i - 1], "time-ordered");
  const perTsdu = new Map<number, number>();
  for (const t of times) perTsdu.set(Math.round(t * 1000), (perTsdu.get(Math.round(t * 1000)) ?? 0) + 1);
  for (const n of perTsdu.values()) assert.ok(n <= 3, "no frame decoded twice");
  assert.ok(status.good > 120, `good TSBKs ${status.good}`);
  assert.equal(status.bad, 0);
});

test("an encrypted call is monitored, not recorded", () => {
  const u8 = synthSystem({
    fs: FS,
    centerHz: CENTER,
    ccHz: CC,
    durationS: 7,
    calls: [{ hz: 773_250_000, tg: 202, src: 777, startS: 0.8, frames: 90, algid: 0x84 }],
  });
  const { started, done } = run(u8);
  assert.equal(started.length, 1);
  // The synthetic grant does not set the encrypted service-option bit, so the
  // call is recorded until the voice channel reveals the ALGID — then no audio
  // may be written.
  assert.equal(done.length, 0, "no audio file for an encrypted call");
});

test("a grant outside the dongle's bandwidth is monitored as no_source", () => {
  const u8 = synthSystem({
    fs: FS,
    centerHz: CENTER,
    ccHz: CC,
    durationS: 3,
    calls: [{ hz: 775_000_000, tg: 303, src: 1, startS: 0.5, frames: 36 }],
  });
  const { started } = run(u8);
  assert.equal(started[0]?.state, "monitoring");
  assert.equal(started[0]?.reason, "no_source");
});
