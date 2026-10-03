// The interface protocol (src/protocol.ts) as JSON Schema: docs/api/
// protocol.schema.json, which the server serves at /api/schema and its tests
// check every message against. protocol.ts is the source; run `npm run
// schema` after changing it (`npm run schema -- --check` fails if stale).

import { createGenerator } from "ts-json-schema-generator";
import { readFileSync, writeFileSync } from "node:fs";

const out = new URL("../../docs/api/protocol.schema.json", import.meta.url);
const gen = createGenerator({
  path: new URL("../src/protocol.ts", import.meta.url).pathname,
  tsconfig: new URL("../tsconfig.json", import.meta.url).pathname,
  skipTypeCheck: true,
  additionalProperties: false,
  expose: "export",
  topRef: true,
  jsDoc: "extended",
});

const definitions = {};
for (const type of ["FromRecorder", "ToRecorder"]) Object.assign(definitions, gen.createSchema(type).definitions);
const sorted = Object.fromEntries(Object.entries(definitions).sort(([a], [b]) => a.localeCompare(b)));
const schema = {
  $schema: "http://json-schema.org/draft-07/schema#",
  title: "Trunk Recorder Pro interface protocol",
  description:
    "The JSON messages on the WebSocket /api/ws. FromRecorder: server → client; ToRecorder: client → server. " +
    "Generated from web/src/protocol.ts — see docs/api/README.md.",
  definitions: sorted,
};
const text = JSON.stringify(schema, null, 2) + "\n";

if (process.argv.includes("--check")) {
  let old = "";
  try {
    old = readFileSync(out, "utf8");
  } catch {}
  if (old !== text) {
    console.error("docs/api/protocol.schema.json is out of date with src/protocol.ts: run `npm run schema` in web/.");
    process.exit(1);
  }
} else writeFileSync(out, text);
