// Signing in, in a real browser: headless Chrome, driven over the Chrome
// DevTools Protocol (no library: Node's WebSocket), with a virtual
// authenticator (WebAuthn.addVirtualAuthenticator: CTAP2, built in, resident
// keys, user verification) so passkeys are made and used unattended, against
// the real trunk-pro binary on a config of its own. What it proves that the
// unit tests (a software key in Rust) can't: the dashboard's and the sign-in
// page's own WebAuthn code with a browser's, the cookie as a browser keeps
// it, and the pages each kind of user sees.
//
//   npm run build && cargo build -p trunk-pro && npm run test:browser
//
// TRUNK_PRO: the binary (default ../target/debug/trunk-pro); CHROME: the
// browser (default: Google Chrome or Chromium where they're installed).
// No Chrome: skipped, saying so. KEEP=1 keeps the temporary folder.

import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const BIN = resolve(process.env.TRUNK_PRO ?? join(here, "../../target/debug/trunk-pro"));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function findChrome() {
  if (process.env.CHROME) return existsSync(process.env.CHROME) ? process.env.CHROME : null;
  for (const p of ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/Applications/Chromium.app/Contents/MacOS/Chromium", "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe"]) if (existsSync(p)) return p;
  for (const name of ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"]) {
    const r = spawnSync("which", [name], { encoding: "utf8" });
    if (r.status === 0 && r.stdout.trim()) return r.stdout.trim();
  }
  return null;
}

const chrome = findChrome();
if (!chrome) {
  console.log("auth-browser: skipped — no Chrome found (install Google Chrome or Chromium, or set CHROME=/path/to/chrome).");
  process.exit(0);
}
if (!existsSync(BIN)) {
  console.error(`auth-browser: no trunk-pro at ${BIN}: cargo build -p trunk-pro first (after npm run build), or set TRUNK_PRO.`);
  process.exit(1);
}

const freePort = () => new Promise((res) => { const s = createServer().listen(0, "127.0.0.1", () => { const p = s.address().port; s.close(() => res(p)); }); });

// ── the recorder ────────────────────────────────────────────────────────────

const dir = mkdtempSync(join(tmpdir(), "trunk-pro-browser-"));
const config = join(dir, "config.json");
const [port, wall] = [await freePort(), await freePort()];
mkdirSync(join(dir, "wall"));
writeFileSync(join(dir, "wall", "index.html"), "<!doctype html><meta charset=utf-8><title>Wall</title><p>A listen interface's page.</p>");
writeFileSync(config, JSON.stringify({ server: { port, interfaces: { wall: { port: wall, access: "listen", path: "wall" } } }, recording: { captureDir: join(dir, "calls") } }, null, 2));
const cli = (...args) => {
  const r = spawnSync(BIN, ["access", ...args, "--config", config], { encoding: "utf8" });
  if (r.status !== 0) throw new Error(`trunk-pro access ${args.join(" ")}: ${r.stderr}`);
  return r.stdout.trim();
};
const luke = cli("add", "luke");
const kiosk = cli("add", "kiosk", "--access", "listen");
cli("on");

let log = "";
const recorder = spawn(BIN, ["--config", config, "--port", String(port), "--no-open"], { stdio: ["ignore", "pipe", "pipe"] });
recorder.stdout.on("data", (d) => (log += d));
recorder.stderr.on("data", (d) => (log += d));
const origin = `http://localhost:${port}`;
const wallOrigin = `http://localhost:${wall}`;
for (let t = Date.now(); ; await sleep(100)) {
  if (await fetch(`${origin}/api/version`).then((r) => r.ok, () => false)) break;
  if (Date.now() - t > 30_000 || recorder.exitCode !== null) throw new Error(`trunk-pro didn't start:\n${log}`);
}
for (let t = Date.now(); !(await fetch(`${wallOrigin}/api/version`).then((r) => r.ok, () => false)); await sleep(100)) if (Date.now() - t > 10_000) throw new Error(`the wall interface isn't served:\n${log}`);
const home = await fetch(`${origin}/`, { headers: { authorization: `Bearer ${luke}` } }).then((r) => r.text());
if (!/<div id="root"/.test(home)) throw new Error(`${BIN} has no dashboard built in: npm run build in web/, then cargo build -p trunk-pro.`);

// ── the browser ─────────────────────────────────────────────────────────────

const profile = join(dir, "chrome");
const browser = spawn(chrome, ["--headless=new", "--remote-debugging-port=0", `--user-data-dir=${profile}`, "--no-first-run", "--no-default-browser-check", "--disable-gpu", "--disable-extensions", ...(process.getuid?.() === 0 ? ["--no-sandbox"] : []), "about:blank"], { stdio: "ignore" });
let wsUrl = null;
for (let t = Date.now(); !wsUrl; await sleep(100)) {
  const f = join(profile, "DevToolsActivePort");
  if (existsSync(f)) {
    const [p, path] = readFileSync(f, "utf8").split("\n");
    if (p && path) wsUrl = `ws://127.0.0.1:${p}${path}`;
  }
  if (Date.now() - t > 20_000) throw new Error("Chrome didn't open its DevTools port");
}

let failed = 0;
// (Each process waited for before the folder goes: Chrome writes its profile as it quits.)
const ended = (p) => (p.exitCode !== null || p.signalCode !== null ? Promise.resolve() : new Promise((r) => { p.once("exit", r); setTimeout(r, 5000); }));
const cleanup = async () => {
  browser.kill();
  recorder.kill();
  await Promise.all([ended(browser), ended(recorder)]);
  if (process.env.KEEP) return console.log(`kept: ${dir}`);
  try {
    rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  } catch (e) {
    console.log(`# couldn't remove ${dir}: ${e.message}`);
  }
};
process.on("exit", () => { browser.kill(); recorder.kill(); });

const ws = new WebSocket(wsUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
let nextId = 0;
const pending = new Map();
const listeners = new Set();
ws.onmessage = (e) => {
  const m = JSON.parse(e.data);
  if (m.id && pending.has(m.id)) {
    const { res, rej, method } = pending.get(m.id);
    pending.delete(m.id);
    if (m.error) rej(new Error(`${method}: ${m.error.message} ${m.error.data ?? ""}`));
    else res(m.result);
  } else if (m.method) for (const l of listeners) l(m);
};
const send = (method, params = {}, sessionId) => new Promise((res, rej) => { const id = ++nextId; pending.set(id, { res, rej, method }); ws.send(JSON.stringify({ id, method, params, sessionId })); });
const event = (method, sessionId, ms = 15_000) => new Promise((res, rej) => {
  const l = (m) => { if (m.method === method && m.sessionId === sessionId) { listeners.delete(l); clearTimeout(t); res(m.params); } };
  const t = setTimeout(() => { listeners.delete(l); rej(new Error(`no ${method} in ${ms} ms`)); }, ms);
  listeners.add(l);
});

const { targetId } = await send("Target.createTarget", { url: "about:blank" });
const { sessionId: sid } = await send("Target.attachToTarget", { targetId, flatten: true });
await send("Page.enable", {}, sid);
await send("Runtime.enable", {}, sid);
await send("Network.enable", {}, sid);
// (Sign out everywhere asks first: yes.)
listeners.add((m) => m.method === "Page.javascriptDialogOpening" && m.sessionId === sid && send("Page.handleJavaScriptDialog", { accept: true }, sid));
await send("WebAuthn.enable", { enableUI: false }, sid);
const { authenticatorId } = await send("WebAuthn.addVirtualAuthenticator", { options: { protocol: "ctap2", ctap2Version: "ctap2_1", transport: "internal", hasResidentKey: true, hasUserVerification: true, isUserVerified: true, automaticPresenceSimulation: true } }, sid);

const evaluate = async (expression) => {
  const r = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true, userGesture: true }, sid);
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? r.exceptionDetails.text);
  return r.result.value;
};
const goto = async (url) => { const loaded = event("Page.loadEventFired", sid); await send("Page.navigate", { url }, sid); await loaded; };
/** Until `expression` is truthy (a page loading meanwhile is no failure): its value. */
const waitFor = async (expression, what, ms = 15_000) => {
  for (let t = Date.now(); ; await sleep(100)) {
    const v = await evaluate(expression).catch(() => null);
    if (v) return v;
    if (Date.now() - t > ms) throw new Error(`waited ${ms} ms for ${what}`);
  }
};
const click = (text, scope = "document") => evaluate(`(() => { const b = [...${scope}.querySelectorAll("button, a")].find((b) => b.textContent.trim() === ${JSON.stringify(text)}); if (!b) throw new Error("no " + ${JSON.stringify(text)}); b.click(); return true; })()`);
const cookie = async () => (await send("Network.getCookies", { urls: [origin] }, sid)).cookies.find((c) => c.name.startsWith("trp_"));
const credentials = async () => (await send("WebAuthn.getCredentials", { authenticatorId }, sid)).credentials;
const savedPasskeys = (user) => JSON.parse(readFileSync(config, "utf8")).server.auth.users[user].passkeys ?? [];
const onSignIn = `document.title === "Sign in" && !!document.getElementById("t")`;
const onDashboard = (user) => `!!document.querySelector(".topbar") && [...document.querySelectorAll(".user-menu button, .user-menu span")].some((b) => b.textContent.trim() === ${JSON.stringify(user)})`;
const navLabels = () => evaluate(`[...document.querySelectorAll(".nav-label")].map((n) => n.textContent.trim())`);
const buttons = () => evaluate(`[...document.querySelectorAll("button")].map((b) => b.textContent.trim())`);
const signInWithToken = async (token) => {
  await waitFor(onSignIn, "the sign-in page");
  await evaluate(`document.getElementById("t").value = ${JSON.stringify(token)}; document.getElementById("f").requestSubmit(); true`);
};

async function step(name, f) {
  try {
    await f();
    console.log(`ok - ${name}`);
  } catch (e) {
    failed++;
    console.log(`not ok - ${name}\n  ${String(e?.stack ?? e).split("\n").join("\n  ")}`);
    throw e;
  }
}

try {
  await step("the sign-in page in place of the dashboard, with Passkey (localhost, this port)", async () => {
    await goto(`${origin}/`);
    await waitFor(onSignIn, "the sign-in page");
    await waitFor(`!document.getElementById("p").hidden`, "the Passkey button");
    if (!(await evaluate(`document.getElementById("pl").hidden`))) throw new Error("a link as well as the button");
  });

  await step("an Admin signs in with their token: Setup and Start are there", async () => {
    await signInWithToken(luke);
    await waitFor(onDashboard("luke"), "the dashboard as luke");
    const nav = await waitFor(`(() => { const l = [...document.querySelectorAll(".nav-label")].map((n) => n.textContent.trim()); return l.includes("Setup") && l; })()`, "Setup in the navigation");
    if (!nav.includes("Setup")) throw new Error(`nav: ${nav}`);
    const c = await cookie();
    if (!c || !c.httpOnly || c.sameSite !== "Lax" || c.secure || !c.value.startsWith("luke.")) throw new Error(`cookie: ${JSON.stringify(c)}`);
    await waitFor(`[...document.querySelectorAll("button")].some((b) => b.textContent.trim() === "Start")`, "Start");
  });

  await step("a passkey added from the top bar: made by the browser, saved by the recorder", async () => {
    await click("luke", `document.querySelector(".user-menu")`);
    await waitFor(`!!document.querySelector('[aria-label="Your sign-in"]')`, "the popover");
    await click("Add passkey");
    const name = await waitFor(`document.querySelector('input[aria-label="Passkey name"]')?.value`, "the passkey's name, prefilled");
    if (!/Chrome/.test(name)) throw new Error(`its name: ${name}`);
    await click("Add");
    const added = await waitFor(`document.querySelector('[aria-label="Passkey added"]')?.textContent`, "Added");
    const creds = await credentials();
    if (creds.length !== 1 || !creds[0].isResidentCredential || creds[0].rpId !== "localhost") throw new Error(`authenticator: ${JSON.stringify(creds)}`);
    const saved = savedPasskeys("luke");
    if (saved.length !== 1 || saved[0].name !== name || !added.includes(name)) throw new Error(`saved: ${JSON.stringify(saved)} shown: ${added}`);
    await waitFor(`[...document.querySelectorAll(".user-keys li")].some((li) => li.textContent.includes(${JSON.stringify(name)}))`, "it, listed");
  });

  /** The session cookie's value once it's a passkey's (luke's), within `ms`. */
  const passkeySession = async (ms) => {
    for (let t = Date.now(); Date.now() - t < ms; await sleep(100)) {
      const c = await cookie();
      if (c?.value.startsWith("luke.pk%3A")) return c.value;
    }
    return null;
  };
  const countKept = async () => {
    const [cred] = await credentials();
    const counter = savedPasskeys("luke")[0].key.cred.counter;
    if (cred.signCount < 1 || counter !== cred.signCount) throw new Error(`counter: the authenticator's ${cred.signCount}, the config's ${counter}`);
  };

  await step("signed out, then in by the passkey the token field's autofill offers (conditional UI), nothing typed", async () => {
    // (The virtual authenticator answers a conditional request at once, as a person picking the passkey from the autofill would.)
    await click("Sign out");
    if (!(await passkeySession(10_000))) throw new Error("no passkey session");
    await waitFor(onDashboard("luke"), "the dashboard as luke, by passkey");
    await countKept();
  });

  await step("signed out, then in with the Passkey button", async () => {
    // (Nobody there to answer, until the button's pressed: the autofill's request waits, then the button's replaces it.)
    await send("WebAuthn.setAutomaticPresenceSimulation", { authenticatorId, enabled: false }, sid);
    await click("Sign out");
    await waitFor(onSignIn, "the sign-in page");
    await waitFor(`!document.getElementById("p").hidden`, "the Passkey button");
    await sleep(1000);
    if (await evaluate(onDashboard("luke"))) throw new Error("signed in with nobody there");
    await send("WebAuthn.setAutomaticPresenceSimulation", { authenticatorId, enabled: true }, sid);
    await click("Passkey");
    await waitFor(onDashboard("luke"), "the dashboard as luke, by passkey");
    if (!(await passkeySession(1000))) throw new Error("not a passkey's session");
    await countKept();
  });

  await step("an answer made on a listen interface's page, sent on to the recorder's port, signs nobody in", async () => {
    const started = await fetch(`${origin}/api/passkey/login/start`, { method: "POST" }).then((r) => r.json());
    await goto(`${wallOrigin}/`);
    const answer = await evaluate(`(async (o) => {
      const bytes = (s) => Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((s.length + 3) % 4)), (c) => c.charCodeAt(0));
      const b64 = (b) => btoa(String.fromCharCode(...new Uint8Array(b))).replace(/\\+/g, "-").replace(/\\//g, "_").replace(/=+$/, "");
      const pk = { ...o.publicKey, challenge: bytes(o.publicKey.challenge) };
      const c = await navigator.credentials.get({ publicKey: pk });
      const a = c.response;
      return { id: c.id, rawId: b64(c.rawId), type: c.type, extensions: {}, response: { authenticatorData: b64(a.authenticatorData), clientDataJSON: b64(a.clientDataJSON), signature: b64(a.signature), userHandle: a.userHandle ? b64(a.userHandle) : null } };
    })(${JSON.stringify(started.options)})`);
    if (!JSON.parse(Buffer.from(answer.response.clientDataJSON, "base64url")).origin.endsWith(`:${wall}`)) throw new Error("the browser signed for the interface's origin");
    const r = await fetch(`${origin}/api/passkey/login/finish`, { method: "POST", body: JSON.stringify({ id: started.id, credential: answer }) });
    const why = await r.text();
    if (r.status !== 401 || r.headers.get("set-cookie") || !why.includes(`work at ${origin} alone`)) throw new Error(`${r.status} ${why}`);
  });

  await step("an interface's sign-in page links to the recorder's for passkeys; on 127.0.0.1, none", async () => {
    await goto(`${wallOrigin}/signin`);
    const href = await waitFor(`!document.getElementById("pl").hidden && document.getElementById("pl").href`, "the link");
    if (href !== `${origin}/signin` || !(await evaluate(`document.getElementById("p").hidden`))) throw new Error(`link ${href}, and no ceremony here`);
    await goto(`http://127.0.0.1:${port}/signin`);
    await sleep(1500);
    if (!(await evaluate(`document.getElementById("p").hidden && document.getElementById("pl").hidden`))) throw new Error("passkeys offered on an IP address");
  });

  await step("the passkey removed: the browser it signed in is signed out", async () => {
    await goto(`${origin}/`);
    await waitFor(onDashboard("luke"), "the dashboard as luke");
    const admin = new WebSocket(`ws://localhost:${port}/api/ws?token=${luke}`);
    const said = [];
    admin.onmessage = (e) => said.push(JSON.parse(e.data));
    for (let t = Date.now(); !said.some((m) => m.type === "hello"); await sleep(50)) if (Date.now() - t > 10_000) throw new Error(`no hello: ${JSON.stringify(said)}`);
    const { config: c, version } = said.find((m) => m.type === "config");
    c.server.auth.users.luke.passkeys = [];
    admin.send(JSON.stringify({ type: "setConfig", config: c, version }));
    for (let t = Date.now(); !said.some((m) => m.type === "configSaved"); await sleep(50)) if (Date.now() - t > 10_000) throw new Error("not saved");
    const saved = said.find((m) => m.type === "configSaved");
    admin.close();
    if (saved.error) throw new Error(saved.error);
    await waitFor(onSignIn, "the sign-in page, its session ended", 20_000);
    const s = await fetch(`${origin}/api/session`, { headers: { cookie: `${(await cookie()).name}=${(await cookie()).value}` } }).then((r) => r.json());
    if (s.user !== null) throw new Error(JSON.stringify(s));
  });

  await step("Sign out everywhere, from the top bar: this browser, and the session copied from it", async () => {
    await signInWithToken(luke);
    await waitFor(onDashboard("luke"), "the dashboard as luke");
    const c = await cookie();
    await click("luke", `document.querySelector(".user-menu")`);
    await click("Sign out everywhere");
    await waitFor(onSignIn, "the sign-in page");
    const s = await fetch(`${origin}/api/session`, { headers: { cookie: `${c.name}=${c.value}` } }).then((r) => r.json());
    if (s.user !== null) throw new Error(`the copied cookie: ${JSON.stringify(s)}`);
  });

  await step("a View user: no Setup, Plugins or Interfaces, no Start or Stop", async () => {
    await goto(`${origin}/signin`);
    await signInWithToken(kiosk);
    await waitFor(onDashboard("kiosk"), "the dashboard as kiosk");
    await waitFor(`document.querySelectorAll(".nav-label").length > 0`, "the navigation");
    await sleep(500);
    const nav = await navLabels();
    const b = await buttons();
    if (nav.some((n) => ["Setup", "Plugins", "Interfaces"].includes(n)) || b.some((x) => ["Start", "Stop", "Setup guide"].includes(x))) throw new Error(`nav ${nav}; buttons ${b}`);
    if (!nav.includes("Overview") || !b.includes("Sign out")) throw new Error(`nav ${nav}; buttons ${b}`);
  });
} catch {
  console.log(`\n# trunk-pro's log (last lines, the radios' own left out):\n${log.split("\n").filter((l) => !/\(info\)\s+(Opening device|starting event loop|event loop thread)/.test(l)).slice(-30).join("\n")}`);
} finally {
  ws.close();
}
await cleanup();
console.log(failed ? `\n${failed} failed` : "\nall passed");
process.exit(failed ? 1 : 0);
