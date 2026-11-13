// The recorder's sign-in page (crates/trunk-pro/src/signin.html, served
// alone, before signing in): the token field, and a Passkey button hidden
// until the session says passkeys work here; its script parses, and its
// base64url helpers are WebAuthn's (no padding, - and _). `npm test`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const page = readFileSync(new URL("../../crates/trunk-pro/src/signin.html", import.meta.url), "utf8");
const script = /<script>([\s\S]*)<\/script>/.exec(page)?.[1] ?? "";

test("Sign-in page: Passkey above the token, hidden until the session says; the token field offers passkeys too", () => {
  assert.match(page, /<button id="p" type="button" hidden>Passkey<\/button>[\s\S]*<input id="t" type="password"/);
  assert.match(page, /autocomplete="current-password webauthn"/, "conditional mediation needs webauthn in autocomplete");
  assert.match(script, /fetch\("\/api\/session"[\s\S]*s\.passkeys/);
  assert.match(script, /"\/api\/passkey\/login\/start"[\s\S]*"\/api\/passkey\/login\/finish"/);
  assert.match(script, /isConditionalMediationAvailable/);
  assert.doesNotMatch(page, /<script src|<link /, "nothing from elsewhere");
  new Function(script);
});

test("Sign-in page: base64url both ways, unpadded", () => {
  const helpers = script.split("\n").filter((l) => /const (bytes|b64url) =/.test(l)).join("\n");
  const { bytes, b64url } = new Function(`${helpers}; return { bytes, b64url };`)() as { bytes: (s: string) => Uint8Array; b64url: (b: ArrayBuffer) => string };
  for (const n of [0, 1, 2, 3, 16, 31, 32, 65]) {
    const b = Uint8Array.from({ length: n }, (_, i) => (i * 37 + 250) & 255);
    const s = b64url(b.buffer);
    assert.doesNotMatch(s, /[=+/]/);
    assert.deepEqual([...bytes(s)], [...b], `${n} bytes`);
  }
  assert.equal(b64url(Uint8Array.from([0xfb, 0xff]).buffer), "-_8");
});
