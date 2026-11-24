// A plugin's settings form, drawn from its schema: a number marked
// `x-slider` is a slider with its value beside it (saved once let go, as
// Setup's gain sliders are), and a `readOnly` field — what the plugin saves
// itself — is shown, not edited; a web address with Open and Copy buttons.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { SchemaFields } from "./Plugins.tsx";
import type { PluginSchema } from "./protocol.ts";

const text = (html: string) => html.replace(/<!-- -->/g, "").replace(/<[^>]+>/g, " ").replace(/\s+/g, " ");

test("plugin form: a slider in its steps, and read-only fields the plugin fills in", () => {
  const schema: PluginSchema = {
    type: "object",
    properties: {
      gainDb: { type: "number", title: "Gain", minimum: -12, maximum: 12, multipleOf: 0.5, "x-slider": true },
      serverName: { type: "string", title: "Server", readOnly: true },
      invite: { type: "string", title: "Invite", format: "uri", readOnly: true, "x-open": "Invite the bot" },
      channel: { type: "string", title: "Channel", readOnly: true },
    },
  };
  const html = renderToString(<SchemaFields schema={schema} values={{ gainDb: 3, serverName: "LolWut", invite: "https://example.com/invite" }} onChange={() => {}} id="p" />);
  assert.match(html, /<input[^>]*type="range"[^>]*min="-12"[^>]*max="12"[^>]*step="0.5"[^>]*value="3"/, html);
  const page = text(html);
  assert.match(page, /Gain \+3\.0/, "its value beside it, signed when the range is");
  assert.match(page, /Server LolWut/);
  assert.doesNotMatch(html, /<input[^>]*value="LolWut"/, "shown, not an input");
  assert.match(page, /Invite Invite the bot ↗ https:\/\/example\.com\/invite Copy/);
  assert.match(page, /Channel Not known yet/);
});

test("plugin form: a secret set above isn't in the placeholder, which a password box shows as it is", () => {
  const schema: PluginSchema = { type: "object", properties: { apiKey: { type: "string", title: "API key", "x-secret": true } } };
  const inherited = renderToString(<SchemaFields schema={schema} values={{}} inherited={{ apiKey: "KEY-FROM-SYSTEM" }} onChange={() => {}} id="p" />);
  assert.doesNotMatch(inherited, /KEY-FROM-SYSTEM/, inherited);
  assert.match(inherited, /<input[^>]*type="password"[^>]*placeholder="•+ \(set above\)"/, inherited);
  const own = renderToString(<SchemaFields schema={schema} values={{ apiKey: "OWN-KEY" }} onChange={() => {}} id="p" />);
  assert.match(own, /<input[^>]*type="password"[^>]*value="OWN-KEY"/, "its own value is in a password box, masked");
  const none = renderToString(<SchemaFields schema={schema} values={{}} onChange={() => {}} id="p" />);
  assert.match(none, /<input[^>]*type="password"[^>]*placeholder=""/, none);
});

test("plugin form: a secret is dots wherever it's shown, as an item's summary too", async () => {
  const { summarize, summaryValue, SECRET_SHOWN } = await import("./pluginForm.ts");
  const key: PluginSchema = { type: "string", title: "API key", "x-secret": true };
  assert.equal(summaryValue(key, "KEY-1"), SECRET_SHOWN);
  assert.equal(summaryValue(key, ""), "", "nothing set: nothing shown");
  const site: PluginSchema = { type: "object", properties: { name: { type: "string" }, apiKey: key }, "x-summary": ["{name}", "{apiKey}"] };
  const line = summarize(site, { name: "North", apiKey: "KEY-NORTH" });
  assert.equal(line, `North · ${SECRET_SHOWN}`);
  assert.doesNotMatch(line, /KEY-NORTH/);
});

// A list a plugin fills in from a service (here, channels it looked up), saved as read-only settings.
const channels = (readOnly: boolean): PluginSchema => ({
  type: "object",
  properties: {
    channels: {
      type: "array",
      title: "Channels",
      readOnly: readOnly || undefined,
      items: {
        type: "object",
        title: "Channel",
        "x-title": "name",
        "x-summary": ["{kind}", "{detail}", "{notes}"],
        properties: { name: { type: "string" }, kind: { type: "string" }, detail: { type: "string" }, notes: { type: "string" } },
      },
    },
  },
});

test("plugin form: a read-only list is rows to read, with nothing to add, edit or remove", () => {
  const values = {
    channels: [
      { name: "Alpha", kind: "Voice", detail: "851.0125 MHz" },
      { name: "Bravo", kind: "Data", detail: "852.5 MHz", notes: "Encrypted: can be listed, not decoded." },
      { name: "", kind: "Voice" },
    ],
  };
  const html = renderToString(<SchemaFields schema={channels(true)} values={values} onChange={() => {}} id="p" />);
  const page = text(html);
  assert.match(page, /Alpha Voice · 851\.0125 MHz/);
  assert.match(page, /Bravo Data · 852\.5 MHz · Encrypted: can be listed, not decoded\./, "its whole line, notes too");
  assert.match(page, /Channel 3 Voice/, "an untitled item is numbered");
  assert.match(page, /3 channels/, "counted");
  assert.doesNotMatch(html, /<button/, "no Add, Edit or Remove");
  assert.match(text(renderToString(<SchemaFields schema={channels(true)} values={{}} onChange={() => {}} id="p" />)), /None yet\./);
});

test("plugin form: a list that isn't read-only still has its buttons", () => {
  const html = renderToString(<SchemaFields schema={channels(false)} values={{ channels: [{ name: "Alpha" }] }} onChange={() => {}} id="p" />);
  assert.match(text(html), /Alpha Edit/, "an item to open and edit");
  assert.match(text(html), /Add channel/);
});