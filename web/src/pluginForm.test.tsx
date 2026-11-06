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
