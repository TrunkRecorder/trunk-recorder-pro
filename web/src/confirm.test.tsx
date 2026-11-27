// Asking before something is done: the app's one dialog (Confirm.tsx) and its questions
// (asking.ts), in place of the browser's confirm(). `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { answer, asking, confirmed } from "./asking.ts";
import { Asking } from "./Confirm.tsx";

const text = (html: string) => html.replace(/<!-- -->/g, "").replace(/<[^>]+>/g, " ").replace(/&#x27;/g, "'").replace(/\s+/g, " ");

test("A question waits for its answer, one at a time: Cancel is no, the action yes", async () => {
  assert.equal(asking(), null);
  assert.equal(renderToString(<Asking />), "", "nothing asked: nothing shown");
  const first = confirmed({ title: "Remove Source 2?", question: "Its settings go with it.", action: "Remove" });
  const second = confirmed({ title: "Compact the store?", action: "Compact", safe: true });
  const html = renderToString(<Asking />);
  assert.match(text(html), /Remove Source 2\? Its settings go with it\. Cancel Remove/, "Cancel first");
  assert.match(html, /<button class="btn danger">Remove<\/button>/);
  answer(false);
  assert.equal(await first, false);
  assert.match(renderToString(<Asking />), /Compact the store\?.*<button class="btn">Compact<\/button>/, "the next one, its button not marked dangerous");
  answer(true);
  assert.equal(await second, true);
  assert.equal(asking(), null);
});

test("A question's blank lines make paragraphs", () => {
  void confirmed({ title: "Give Trunkamp control?", question: "With control it can do anything.\n\nGive control only to interfaces you trust.", action: "Give control" });
  assert.match(renderToString(<Asking />), /<p>With control it can do anything\.<\/p><p>Give control only to interfaces you trust\.<\/p>/);
  answer(false);
});
