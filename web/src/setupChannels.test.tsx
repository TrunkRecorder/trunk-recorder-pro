// Setup's conventional channel table as rendered (react-dom/server): what
// each row records, as chips, and what it files under. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { writeFileSync } from "node:fs";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { defaultConfig, newConventional } from "./config.ts";
import { transport } from "./controller.ts";
import type { Channel } from "./protocol.ts";
import { ConventionalPanel } from "./Setup.tsx";

const text = (html: string) => html.replace(/<!-- -->/g, "").replace(/<[^>]+>/g, " ").replace(/&#x27;/g, "'").replace(/\s+/g, " ");
const row = (freqHz: number, mode: Channel["mode"], more: Partial<Channel> = {}): Channel => ({ freqHz, mode, name: "", enabled: true, ...more });

test("Setup's channels: each row's code, slot and talkgroup as chips, and what it files under", () => {
  const c = defaultConfig();
  c.conventional.push(
    newConventional(c, {
      shortName: "town",
      channels: [
        row(154_325_000, "fm", { code: "D223N", name: "County A Fire" }),
        row(154_325_000, "fm", { name: "154.325 (other)" }),
        row(460_125_000, "p25", { talkgroup: 46012, name: "Townville PD" }),
        row(453_500_000, "p25", { code: "293", matchTalkgroup: 1, talkgroup: 45351, name: "Townville PW" }),
        row(453_500_000, "p25", { code: "293", name: "Townville (other)" }),
        row(451_500_000, "dmr", { code: "1", slot: 1, matchTalkgroup: 1, talkgroup: 9001, name: "Dispatch" }),
        row(462_550_000, "dmr", { code: "CC 99", name: "Yard" }),
      ],
    }),
  );
  for (const m of [{ type: "config", config: c, version: 1 }, { type: "hello", version: "test", platform: "test", api: 1, surveyBands: [], capabilities: ["service"] }] as const) {
    transport.onMessage(m as never);
  }
  const html = renderToString(<ConventionalPanel c={c} k={0} />);
  if (process.env.SETUP_HTML) writeFileSync(process.env.SETUP_HTML, html);
  const chips = [...html.matchAll(/class="match-line[^"]*"[^>]*>(.*?)<\/button>/g)].map((m) => text(m[1]).trim());
  assert.deepEqual(chips, [
    "D223N",
    "everything else",
    "everything",
    "NAC 293 TG 1",
    "NAC 293",
    "CC 1 slot 1 TG 1",
    "CC 99",
  ]);
  // A code that doesn't read: marked, and its editor open to fix it.
  assert.ok(html.includes('class="match-chip k-code bad"') && html.includes('class="rule-edit"'));
  assert.equal(html.match(/class="rule-edit"/g)?.length, 1, "the others' editors stay shut");
  assert.ok(html.includes('class="match-chip k-tg"') && html.includes('class="match-chip k-slot"') && html.includes('class="match-chip k-code"'));
  assert.match(text(html), /Each row records tone \/ DCS NAC \/ colour code slot talkgroup everything else/);
  assert.match(text(html), /CSV format .* Match TG p25, dmr, nxdn: only transmissions naming this talkgroup/);
});
