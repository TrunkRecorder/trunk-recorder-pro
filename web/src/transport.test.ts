// The connection to the recorder (transport.ts): a tab that wasn't running
// doesn't show what was queued for it as live, it reconnects. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { WsTransport } from "./transport.ts";

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
