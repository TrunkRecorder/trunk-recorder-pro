// The page an app opens in the browser to be signed in
// (crates/trunk-pro/src/app-signin.html, served alone): who's signed in
// allows it, and the browser goes on to the app's link (or shows the code
// to copy). Its script parses; what the app says is only ever text. `npm test`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const page = readFileSync(new URL("../../crates/trunk-pro/src/app-signin.html", import.meta.url), "utf8");
const script = /<script>([\s\S]*)<\/script>/.exec(page)?.[1] ?? "";
/** A one-line helper of the page's, by name. */
const helper = <T>(name: string): T => new Function(`${script.split("\n").find((l) => l.includes(`const ${name} =`)) ?? ""}; return ${name};`)() as T;

test("App sign-in page: Allow is off until the recorder has read what the app asks", () => {
  assert.match(page, /<button id="yes" type="button" disabled>Allow<\/button>/);
  assert.match(script, /fetch\("\/api\/app\/ask\?" \+ request[\s\S]*if \(!r\.ok\) return[\s\S]*\$\("yes"\)\.disabled = false/, "refused: why, and no Allow");
  assert.match(script, /const request = location\.search\.slice\(1\)/, "the recorder reads the ask, not the page");
  assert.match(script, /fetch\("\/api\/app\/approve"[\s\S]*JSON\.stringify\(\{ request, access \}\)[\s\S]*location\.replace\(a\.to\)/);
  assert.match(script, /\$\("code"\)\.textContent = a\.code/, "no link from the app: the code, to copy");
  assert.match(script, /if \(cancel\) location\.replace\(cancel\)/, "Cancel tells the app, by the recorder's link");
  assert.doesNotMatch(page, /<script src|<link /, "nothing from elsewhere");
  new Function(script);
});

test("App sign-in page: what the app and the recorder say is shown as text", () => {
  assert.doesNotMatch(script, /innerHTML|insertAdjacentHTML|document\.write/);
  const role = helper<(a: string | null) => string>("role");
  assert.deepEqual([role("control"), role("listen"), role(null)], ["Admin", "View", "None"]);
});

test("App sign-in page: where the code goes, and an unencrypted page, are told", () => {
  const returns = helper<(a: { returns: string; host?: string }) => string>("returns");
  assert.deepEqual([returns({ returns: "device" }), returns({ returns: "web", host: "app.example.com" }), returns({ returns: "paste" })], ["This device", "app.example.com", "Copy a code"]);
  const unencrypted = helper<(loc: { protocol: string; hostname: string }) => boolean>("unencrypted");
  const at = (protocol: string, hostname: string) => unencrypted({ protocol, hostname });
  assert.deepEqual([at("http:", "192.168.1.20"), at("http:", "radio.lan"), at("https:", "radio.example.com"), at("http:", "localhost"), at("http:", "127.0.0.1"), at("http:", "[::1]")], [true, true, false, false, false, false]);
  assert.match(page, /<dd id="plain" class="warn" hidden>Unencrypted<\/dd>/);
  assert.match(script, /if \(a\.access === "control"\)[\s\S]*\$\("pick"\)\.hidden = false/, "an Admin may allow it less");
});
