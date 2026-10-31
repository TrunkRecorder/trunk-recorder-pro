// Setup's Talkgroups field: what it says of a system's talkgroups (their
// count, those ignored or encrypted, columns kept that Trunk Recorder doesn't
// know). `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { transport } from "./controller.ts";
import { TalkgroupsField } from "./TalkgroupsField.tsx";
import { readTalkgroupFile } from "./talkgroups.ts";

test("a system's talkgroups: how many, ignored, encrypted, and unknown columns kept", () => {
  const rows = readTalkgroupFile("Decimal;Mode;Alpha Tag;Description;Zone;Ignore\n101;D;Dispatch;;North;\n102;DE;Tac;;;\n103;D;Data;;;x\n").talkgroups;
  transport.onMessage({ type: "talkgroups", system: "tgfield", talkgroups: rows });
  const html = renderToString(<TalkgroupsField sys={{ shortName: "tgfield" }} />);
  assert.match(html, /<span class="mono">3<\/span>/);
  assert.match(html, /1<!-- --> ignored/);
  assert.match(html, /1<!-- --> encrypted/);
  assert.match(html, /unknown column<!-- -->: <!-- -->Zone/);
  assert.match(renderToString(<TalkgroupsField sys={{ shortName: "none" }} />), /<span class="mono">none<\/span>/);
});
