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
        row(460_125_000, "p25", { talkgroup: 46012, name: "Townville PD", tag: "Law Dispatch", group: "Police" }),
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
  // Names beside the alpha tag: shown under it, edited in the row's editor, the system's own offered.
  const meta = [...html.matchAll(/class="row-meta small"[^>]*>(.*?)<\/button>/g)].map((m) => text(m[1]).trim());
  assert.deepEqual(meta, ["Law Dispatch · Police"]);
  assert.equal(html.match(/title="Its codes, and its description, tag and category"/g)?.length, 7, "every row has an Edit");
  const editor = html.slice(html.indexOf('class="rule-edit"'));
  assert.match(text(editor), /Records .* Names Description Tag Category/);
  const tagList = editor.match(/list="([^"]+-tag)"/)?.[1];
  assert.ok(tagList && html.includes(`<datalist id="${tagList}"><option value="Law Dispatch"></option></datalist>`), "the system's tags offered");
});

test("Setup's channels: a digital row can correct its source's AutoTune; an analog row can't", () => {
  const c = defaultConfig();
  c.conventional.push(
    newConventional(c, {
      shortName: "town",
      channels: [
        // (A code that doesn't read opens the row's editor.)
        row(453_375_000, "p25", { code: "NAC?", name: "Town PD", autoTune: true }),
        row(453_225_000, "fm", { code: "999.9", name: "Town PW", autoTune: true }),
      ],
    }),
  );
  transport.onMessage({ type: "config", config: c, version: 1 } as never);
  const html = renderToString(<ConventionalPanel c={c} k={0} />);
  const labels = [...html.matchAll(/class="row-meta small muted"[^>]*>(.*?)<\/span>/g)].map((m) => text(m[1]).trim());
  assert.deepEqual(labels, ["AutoTune"], "the P25 row's, not the FM row's");
  const editors = html.split('class="rule-edit"').slice(1);
  assert.equal(editors.length, 2);
  assert.match(text(editors[0]), /AutoTune Correct the source from its calls/);
  assert.match(editors[0], /type="checkbox" checked=""/);
  assert.doesNotMatch(text(editors[1]), /Correct the source/);
  assert.match(text(html), /AutoTune p25, dmr, nxdn: true lets its calls correct/);
});
