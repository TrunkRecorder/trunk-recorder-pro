// The page an app opens in the browser to be signed in
// (crates/trunk-pro/src/app-signin.html, served alone): who's signed in
// allows it, and the browser goes on to the app's link. Its script parses;
// the app's name is only ever text. `npm test`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const page = readFileSync(new URL("../../crates/trunk-pro/src/app-signin.html", import.meta.url), "utf8");
const script = /<script>([\s\S]*)<\/script>/.exec(page)?.[1] ?? "";

test("App sign-in page: Allow is off until the session says a user of this recorder is signed in on its own port", () => {
  assert.match(page, /<button id="yes" type="button" disabled>Allow<\/button>/);
  assert.match(script, /if \(!s\.auth\)[\s\S]*else if \(!s\.appSignIn\)[\s\S]*else if \(s\.proxy\)[\s\S]*else \$\("yes"\)\.disabled = false/);
  assert.match(script, /fetch\("\/api\/app\/approve"[\s\S]*challenge: q\.get\("challenge"\)[\s\S]*location\.replace\(\(await r\.json\(\)\)\.to\)/);
  assert.match(script, /location\.replace\(back \+ "\?error=denied"\)/, "Cancel tells the app");
  assert.doesNotMatch(page, /<script src|<link /, "nothing from elsewhere");
  new Function(script);
});

test("App sign-in page: what the app and the session say is shown as text", () => {
  assert.doesNotMatch(script, /innerHTML|insertAdjacentHTML|document\.write/);
  const line = script.split("\n").find((l) => /const role =/.test(l)) ?? "";
  const role = new Function(`${line}; return role;`)() as (a: string | null) => string;
  assert.deepEqual([role("control"), role("listen"), role(null)], ["Admin", "View", "None"]);
});
