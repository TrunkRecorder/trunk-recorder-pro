// The connection to the recorder (transport.ts): a tab that wasn't running
// doesn't show what was queued for it as live, it reconnects. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { behind, WsTransport } from "./transport.ts";

/** The sockets opened, newest last: each says what it was told and whether it was closed. */
const sockets: Socket[] = [];
class Socket {
  binaryType = "";
  onopen: (() => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  closed = false;
  constructor() {
    sockets.push(this);
  }
  send(): void {}
  close(): void {
    this.closed = true;
  }
}

test("a message after 30 s of nothing closes the connection, unseen; one after 2 s is taken", (t) => {
  (globalThis as Record<string, unknown>).WebSocket = Socket;
  t.mock.timers.enable({ apis: ["Date"], now: 1_000_000 });
  const tr = new WsTransport("ws://test");
  const seen: unknown[] = [];
  tr.onMessage = (m) => seen.push(m);
  const ws = sockets.at(-1)!;
  ws.onopen!();
  const status = (t: number) => ws.onmessage?.({ data: JSON.stringify({ type: "host", t }) });
  status(1);
  t.mock.timers.tick(2_000);
  status(2);
  assert.equal(seen.length, 2);
  t.mock.timers.tick(31_000);
  status(3);
  assert.equal(seen.length, 2, "the backlog isn't shown");
  assert.ok(ws.closed, "it reconnects instead");
  status(4);
  assert.equal(seen.length, 2, "nor what follows it");
  tr.close();
});

test("host statuses falling behind the server's clock close the connection; a steady lag doesn't", (t) => {
  (globalThis as Record<string, unknown>).WebSocket = Socket;
  t.mock.timers.enable({ apis: ["Date"], now: 1_000_000 });
  const tr = new WsTransport("ws://test");
  const seen: unknown[] = [];
  tr.onMessage = (m) => seen.push(m);
  const ws = sockets.at(-1)!;
  ws.onopen!();
  const status = (t: number) => ws.onmessage?.({ data: JSON.stringify({ type: "host", t }) });
  // The server's clock 5 min off this one: only a change in the lag counts.
  let server = 1_000 + 300;
  for (let i = 0; i < 5; i++, server += 2) {
    status(server);
    t.mock.timers.tick(2_000);
  }
  assert.equal(seen.length, 5);
  assert.ok(!ws.closed);
  // Taken in at half the rate they were sent: each 2 s here is 1 s of the server's.
  for (let i = 0; i < 12 && !ws.closed; i++, server += 1) {
    status(server);
    t.mock.timers.tick(2_000);
  }
  assert.ok(ws.closed, "it reconnects once 10 s behind");
  assert.ok(seen.length < 17, "and stops showing the backlog");
  tr.close();
});

test("behind: more than 10 s past the least lag on the connection", () => {
  assert.ok(!behind(300_000, 310_000), "10 s is still live");
  assert.ok(behind(300_000, 310_001));
  assert.ok(!behind(-5_000, -4_000), "a server clock ahead of this one is no different");
});

test("a reconnected connection measures its lag afresh: the old one's least isn't held against it", (t) => {
  (globalThis as Record<string, unknown>).WebSocket = Socket;
  t.mock.timers.enable({ apis: ["Date", "setTimeout"], now: 1_000_000 });
  const tr = new WsTransport("ws://test");
  const seen: unknown[] = [];
  tr.onMessage = (m) => seen.push(m);
  const open = () => {
    const ws = sockets.at(-1)!;
    ws.onopen!();
    return { ws, status: (t: number) => ws.onmessage?.({ data: JSON.stringify({ type: "host", t }) }) };
  };
  const first = open();
  first.status(1_000);
  t.mock.timers.tick(2_000);
  first.status(1_002 - 20);
  assert.ok(first.ws.closed, "20 s behind: dropped");
  first.ws.onclose!();
  t.mock.timers.tick(500);
  // The server's clock now a minute behind this one (restarted, set back): a steady lag all the same.
  const second = open();
  assert.notEqual(second.ws, first.ws, "it reconnected");
  for (let i = 0, server = 1_002 - 60; i < 5; i++, server += 2) {
    second.status(server);
    t.mock.timers.tick(2_000);
  }
  assert.ok(!second.ws.closed, "no reconnecting over and over");
  assert.equal(seen.length, 6);
  tr.close();
});
