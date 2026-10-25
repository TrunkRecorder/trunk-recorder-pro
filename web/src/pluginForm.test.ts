// Plugin settings forms: lists naming other lists' items, and the items plugins' buttons add. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { fillTemplate, followRenames, refOptions, summarize } from "./pluginForm.ts";
import type { PluginSchema } from "./protocol.ts";

// A notifier's settings: rules naming destinations.
const schema: PluginSchema = {
  type: "object",
  properties: {
    rules: { type: "array", items: { type: "object", properties: { name: { type: "string" }, to: { type: "array", items: { type: "string" }, "x-ref": "destinations" } } } },
    destinations: { type: "array", items: { type: "object", properties: { name: { type: "string" } } } },
  },
};

test("a list's items as choices, by id else name", () => {
  const root = { destinations: [{ name: "Station" }, { id: "hook", name: "Web hook" }, {}] };
  assert.deepEqual(refOptions(root, "destinations"), [
    { value: "Station", label: "Station" },
    { value: "hook", label: "Web hook (hook)" },
  ]);
  assert.deepEqual(refOptions(undefined, "destinations"), []);
});

test("renaming an item renames what names it", () => {
  const prev = { rules: [{ name: "Fire", to: ["Station", "Other"] }], destinations: [{ name: "Station" }, { name: "Other" }] };
  const next = { ...prev, destinations: [{ name: "Station pager" }, { name: "Other" }] };
  const out = followRenames(schema, prev, next);
  assert.deepEqual(out.rules, [{ name: "Fire", to: ["Station pager", "Other"] }]);
  assert.deepEqual(next.rules, prev.rules, "what was passed in is left as it was");
  // Cleared, then typed again: from what it was before it was cleared.
  const memory = {};
  const cleared = { ...prev, destinations: [{ name: "" }, { name: "Other" }] };
  const mid = followRenames(schema, prev, cleared, memory);
  assert.deepEqual(mid.rules, prev.rules, "nothing to rename to yet");
  const typed = followRenames(schema, mid, { ...mid, destinations: [{ name: "Pager" }, { name: "Other" }] }, memory);
  assert.deepEqual(typed.rules, [{ name: "Fire", to: ["Pager", "Other"] }]);
  // Added or removed: nothing to follow.
  const added = { ...prev, destinations: [...prev.destinations, { name: "New" }] };
  assert.equal(followRenames(schema, prev, added), added);
});

test("a contribution's item, filled in", () => {
  const item = { name: "{name}", event: "signal", signals: ["{signal}"], title: "{name} on {system}" };
  assert.deepEqual(fillTemplate(item, { name: "Fire page", signal: "wood:FD_PAGE", system: "wood" }), {
    name: "Fire page",
    event: "signal",
    signals: ["wood:FD_PAGE"],
    title: "Fire page on wood",
  });
  assert.equal(fillTemplate("{unknown}", {}), "{unknown}");
});

test("an item's summary line", () => {
  const rule: PluginSchema = {
    type: "object",
    "x-summary": ["{event}", "{signals}", "{systems|any system}", "TG {talkgroups}", "{followUp}", "→ {to|nowhere yet}"],
    properties: {
      event: { type: "string", enum: ["signal", "call"], "x-enum-labels": ["A signal is heard", "A call begins"], default: "signal" },
      signals: { type: "array", items: { type: "string" }, "x-when": { event: "signal" } },
      systems: { type: "array", items: { type: "string" } },
      talkgroups: { type: "array", items: { type: "string" } },
      followUp: { type: "boolean", title: "Then the recording" },
      to: { type: "array", items: { type: "string" } },
    },
  };
  assert.equal(summarize(rule, { signals: ["wood:FD_PAGE"], systems: ["wood"], to: ["Discord"], followUp: true }), "A signal is heard · wood:FD_PAGE · wood · Then the recording · → Discord");
  assert.equal(summarize(rule, { event: "call", signals: ["wood:FD_PAGE"], talkgroups: ["401"] }), "A call begins · any system · TG 401 · → nowhere yet");
  assert.equal(summarize({ type: "object" }, {}), "");
});

test("tones typed in read as the recorder writes a pattern heard; what names a signal is found anywhere", async () => {
  const { summaryValue, mentions } = await import("./pluginForm.ts");
  const tones: PluginSchema = { type: "array", "x-tones": true, items: { type: "object" } };
  assert.equal(summaryValue(tones, [{ tonesHz: [313, 1642] }, { pattern: "digits", digits: "0123", knox: true }, { tonesHz: [0, 0] }]), "two-tone 313 → 1642 Hz, KNOX 0123");
  assert.ok(mentions([{ name: "Fire", signals: ["wood:FD_PAGE"] }], "wood:FD_PAGE"));
  assert.ok(!mentions([{ name: "wood:FD_PAGE_2" }], "wood:FD_PAGE"), "the whole string, not part of one");
});
