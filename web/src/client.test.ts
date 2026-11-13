// client.js's TrunkClient (docs/api/client.js): what a page made with it can call. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { TrunkClient } from "../../docs/api/client.js";

test("no field of a client hides one of its methods (this.calls once replaced calls(query))", (t) => {
  // (It connects at once, and retries until closed.)
  const rec = new TrunkClient({ server: "localhost:1" });
  t.after(() => rec.close());
  const methods = Object.getOwnPropertyNames(TrunkClient.prototype).filter((m) => m !== "constructor");
  assert.deepEqual(Object.keys(rec).filter((k) => methods.includes(k)), []);
  assert.equal(typeof rec.queryCalls, "function");
  assert.ok(Array.isArray(rec.calls), "rec.calls: the calls on the air");
});
