// The recorder's sign-in page (crates/trunk-pro/src/signin.html, served
// alone, before signing in): the token field, and a Passkey button hidden
// until the session says passkeys work here (else a link to where they do); its script parses, and its
// base64url helpers are WebAuthn's (no padding, - and _). `npm test`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const page = readFileSync(new URL("../../crates/trunk-pro/src/signin.html", import.meta.url), "utf8");
const script = /<script>([\s\S]*)<\/script>/.exec(page)?.[1] ?? "";

test("Sign-in page: Passkey above the token, hidden until the session says; the token field offers passkeys too", () => {
  assert.match(page, /<button id="p" type="button" hidden>Passkey<\/button>[\s\S]*<input id="t" type="password"/);
  assert.match(page, /autocomplete="current-password webauthn"/, "conditional mediation needs webauthn in autocomplete");
  assert.match(script, /fetch\("\/api\/session"[\s\S]*passkeysAt\(s, location\.origin\)/);
  assert.match(script, /"\/api\/passkey\/login\/start"[\s\S]*"\/api\/passkey\/login\/finish"/);
  assert.match(script, /isConditionalMediationAvailable/);
  assert.doesNotMatch(page, /<script src|<link /, "nothing from elsewhere");
  new Function(script);
});

test("Sign-in page: a passkey here only at the origin the recorder says; elsewhere a link to it, never a ceremony", () => {
  assert.match(page, /<a id="pl" class="button" hidden>Passkey ↗<\/a>/);
  const line = script.split("\n").find((l) => /const passkeysAt =/.test(l)) ?? "";
  const passkeysAt = new Function(`${line}; return passkeysAt;`)() as (s: { passkeys?: boolean; passkeyOrigin?: string }, origin: string) => string | null;
  const dash = "https://radio.example.com";
  assert.equal(passkeysAt({ passkeys: true, passkeyOrigin: dash }, dash), "here");
  assert.equal(passkeysAt({ passkeys: false, passkeyOrigin: dash }, "https://radio.example.com:8081"), "there", "an interface's port: the dashboard's page");
  assert.equal(passkeysAt({ passkeys: true, passkeyOrigin: "http://localhost:8080" }, "http://localhost:9000"), "there", "a tunnel's port: where they work");
  assert.equal(passkeysAt({ passkeys: false }, "http://192.168.1.5:8080"), null, "none");
  assert.match(script, /pl\.href = s\.passkeyOrigin \+ "\/signin"/);
  assert.match(script, /if \(at === "there"\) \{[\s\S]*?return;\s*\}\s*if \(at !== "here"/, "a link there, and no ceremony");
  assert.match(script, /if \(!\(await answers\(s\.passkeyOrigin\)\)\) return;\s*pl\.href/, "no link where nothing answers");
  assert.match(script, /const answers = [\s\S]*?"\/api\/version", \{ mode: "no-cors"[\s\S]*?AbortSignal\.timeout\(3000\)/);
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
