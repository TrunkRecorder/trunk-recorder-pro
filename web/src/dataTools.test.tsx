// Setup → Data's export and delete card, and the remove-system dialog, as
// rendered (react-dom/server). `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { exportUrl } from "./controller.ts";
import { deletedText, ManageData, RemoveSystem } from "./DataTools.tsx";

const text = (html: string) => html.replace(/<!-- -->/g, "").replace(/<[^>]+>/g, " ").replace(/&#x27;/g, "'").replace(/\s+/g, " ");

test("Setup → Data: every kind can be ticked to delete, and each table exported as a CSV file", () => {
  const html = renderToString(<ManageData />);
  const page = text(html);
  for (const kind of ["Calls", "Talkgroups", "Talker aliases", "Radio registry", "Dashboard history"]) assert.match(page, new RegExp(kind));
  assert.match(page, /All systems/);
  const links = [...html.matchAll(/href="([^"]+)" download=""/g)].map((m) => m[1]);
  assert.deepEqual(links, ["calls", "transmissions", "talkgroups", "talker-aliases", "radios", "heard-talkgroups", "dashboard-history"].map((t) => `/api/v1/export/${t}`));
  assert.match(html, /<button class="btn danger" disabled="">Delete what&#x27;s ticked…<\/button>/, "nothing ticked yet");
  assert.equal(exportUrl("radios", 3), "/api/v1/export/radios?system=3");
  assert.match(html, /<button class="btn ghost">Compact DB…<\/button>/, "the whole store compacts");
});

test("Removing a system asks whether its data goes too, yes unless unticked", () => {
  const ask = (id: number | undefined) => renderToString(<RemoveSystem name="dcfd" id={id} question="Its recordings stay on disk." onRemove={() => {}} onClose={() => {}} />);
  const html = ask(4);
  assert.match(text(html), /Remove dcfd\? Its recordings stay on disk\. Also delete its data/);
  assert.match(html, /<input type="checkbox" checked=""/);
  assert.doesNotMatch(ask(undefined), /Also delete its data/, "never saved: nothing stored of it");
});

test("What a delete took, in words", () => {
  assert.equal(deletedText({ calls: 1204, talkgroups: 1, talkerAliases: 0, radios: 3, heardTalkgroups: 0, historySeries: 2 }), "1,204 calls, 1 talkgroup, 3 radios, 2 dashboard series");
  assert.equal(deletedText({ calls: 0, talkgroups: 0, talkerAliases: 0, radios: 0, heardTalkgroups: 0, historySeries: 0 }), "nothing");
});
