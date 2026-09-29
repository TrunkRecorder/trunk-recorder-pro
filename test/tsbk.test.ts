// TsbkParser vs Trunk Recorder's p25_parser.cc field layout: blocks are built
// bit-for-bit from the same (shift, mask) positions and parsed back.

import { test } from "node:test";
import assert from "node:assert/strict";
import { TsbkParser } from "../src/protocols/p25/tsbkParser.ts";
import { appendCrc } from "../src/vendor/ff/p25/crc.ts";
import { buildGrant, buildIdenUp, buildNetSts } from "../src/vendor/ff/p25/encode.ts";

/** A TSBK from (shift, value) fields, as p25_parser.cc reads them. */
function tsbk(opcode: number, fields: [number, number][], mfrid = 0): Uint8Array {
  let t = (BigInt(opcode & 0x3f) << 88n) | (BigInt(mfrid) << 80n);
  for (const [shift, v] of fields) t |= BigInt(v) << BigInt(shift);
  const b = new Uint8Array(12);
  for (let i = 0; i < 10; i++) b[i] = Number((t >> BigInt((11 - i) * 8)) & 0xffn);
  return appendCrc(b);
}

function withIden(): TsbkParser {
  const p = new TsbkParser();
  p.parse(buildIdenUp({ iden: 1, baseHz: 851_006_250, stepHz: 6_250, offsetHz: -45_000_000 }), 0x443, 0);
  return p;
}

test("group voice grant resolves its channel through IDEN_UP", () => {
  const p = withIden();
  const [m] = p.parse(buildGrant((1 << 12) | 1057, 2501, 1118028), 0x443, 1.5);
  assert.equal(m.type, "grant");
  assert.equal(m.freqHz, 851_006_250 + 6_250 * 1057);
  assert.equal(m.talkgroup, 2501);
  assert.equal(m.source, 1118028);
  assert.equal(m.phase2Tdma, false);
  assert.equal(m.timeS, 1.5);
  assert.equal(m.nac, 0x443);
});

test("grant service options map to emergency / encrypted / priority", () => {
  const p = withIden();
  const [m] = p.parse(tsbk(0x00, [[72, 0x80 | 0x40 | 0x05], [56, (1 << 12) | 10], [40, 42], [16, 7]]), 0, 0);
  assert.equal(m.emergency, true);
  assert.equal(m.encrypted, true);
  assert.equal(m.priority, 5);
  assert.equal(m.talkgroup, 42);
});

test("grant update carries two channel/group pairs", () => {
  const p = withIden();
  const msgs = p.parse(tsbk(0x02, [[64, (1 << 12) | 1], [48, 100], [32, (1 << 12) | 2], [16, 200]]), 0, 0);
  assert.deepEqual(
    msgs.map((m) => [m.type, m.talkgroup, m.freqHz]),
    [
      ["update", 100, 851_012_500],
      ["update", 200, 851_018_750],
    ],
  );
});

test("IDEN_UP_TDMA makes channels TDMA with slot = channel & 1", () => {
  const p = new TsbkParser();
  // iden 2, channel type 3 (2 slots), spacing 100 (12.5 kHz), base 770 MHz
  p.parse(tsbk(0x33, [[76, 2], [72, 3], [58, (1 << 13) | 0], [48, 100], [16, 770_000_000 / 5]]), 0, 0);
  const [a] = p.parse(buildGrant((2 << 12) | 7, 11053, 1), 0, 0);
  assert.equal(a.phase2Tdma, true);
  assert.equal(a.tdmaSlot, 1);
  assert.equal(a.freqHz, 770_000_000 + 12_500 * 3);
});

test("network status reports WACN / SysID; unknown CC channel stays 'unknown'", () => {
  const p = withIden();
  const [m] = p.parse(buildNetSts(0xbee00, 0x445, (1 << 12) | 5), 0, 0);
  assert.equal(m.type, "status");
  assert.equal(m.wacn, 0xbee00);
  assert.equal(m.sysId, 0x445);
  const [u] = new TsbkParser().parse(buildNetSts(0xbee00, 0x445, (1 << 12) | 5), 0, 0);
  assert.equal(u.type, "unknown");
});

test("Motorola patch add (mfrid 0x90 on opcode 0)", () => {
  const [m] = new TsbkParser().parse(tsbk(0x00, [[64, 9001], [48, 1], [32, 2], [16, 3]], 0x90), 0, 0);
  assert.equal(m.type, "patch_add");
  assert.deepEqual(m.patch, { sg: 9001, ga1: 1, ga2: 2, ga3: 3 });
});

test("unit events: affiliation, registration, deregistration", () => {
  const p = new TsbkParser();
  assert.deepEqual(
    [
      p.parse(tsbk(0x28, [[16, 123456], [40, 2501]]), 0, 0)[0],
      p.parse(tsbk(0x2c, [[40, 654321]]), 0, 0)[0],
      p.parse(tsbk(0x2f, [[16, 777]]), 0, 0)[0],
    ].map((m) => [m.type, m.source, m.talkgroup]),
    [
      ["affiliation", 123456, 2501],
      ["registration", 654321, 0],
      ["deregistration", 777, 0],
    ],
  );
});
