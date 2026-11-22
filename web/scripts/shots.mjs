// The docs' screenshots, taken from the interface as it is now: the real
// trunk-pro binary on a config of its own, playing corpus captures as its
// radios, in headless Chrome (cdp.mjs), each screen in the light and the dark
// theme. The list is shots.config.mjs; the pictures are docs/img/shots/.
//
//   npm run build && cargo build --release -p trunk-pro
//   target/release/trunk-pro tool corpus fetch --only phase2,smartnet    (once)
//   npm run shots                     retake them all (a picture that didn't change isn't rewritten)
//   npm run shots -- --only rf,listen
//   npm run shots:check               CI: every shot still renders (into a temporary folder), every picture
//                                     the docs show is there, and none is left over
//   npm run shots:check -- --release  …and the interface hasn't changed since they were taken (shots.lock.json)
//
// --out <dir> writes there instead; --theme light|dark takes one; --keep keeps
// the temporary folders. TRUNK_PRO: the binary (default ../target/release/trunk-pro);
// CHROME: the browser; CORPUS: the captures (default ../captures/.corpus).
// docs/developer/README.md → Screenshots.

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { ended, findChrome, freePort, logTail, sleep, startChrome, startRecorder } from "./cdp.mjs";
import shots, { scenarios } from "./shots.config.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(here, "../..");
const BIN = resolve(process.env.TRUNK_PRO ?? join(ROOT, "target/release/trunk-pro"));
const CORPUS = resolve(process.env.CORPUS ?? join(ROOT, "captures/.corpus"));
const SHOTS = join(ROOT, "docs/img/shots");
const LOCK = join(SHOTS, "shots.lock.json");
const THEMES = ["light", "dark"];
/** How long a capture plays, and the latest a shot may start in it. */
const REPLAY_S = 90;
const LAST_S = 72;

const argv = process.argv.slice(2);
const flag = (name) => argv.includes(name);
const value = (name) => (argv.includes(name) ? argv[argv.indexOf(name) + 1] : null);
const check = flag("--check");
const only = value("--only")?.split(",") ?? null;
const themes = value("--theme") ? [value("--theme")] : THEMES;
const out = value("--out") ? resolve(value("--out")) : check ? mkdtempSync(join(tmpdir(), "trunk-pro-shots-out-")) : SHOTS;
const unknown = (only ?? []).filter((n) => !shots.some((s) => s.name === n));
if (unknown.length) fail(`no shot named ${unknown.join(", ")} in shots.config.mjs`);

function fail(why) {
  console.error(`shots: ${why}`);
  process.exit(1);
}

// ── what the docs show, against what's there ────────────────────────────────

/** Every Markdown file of the docs, and the README. */
function docFiles(dir = join(ROOT, "docs")) {
  const found = dir === join(ROOT, "docs") ? [join(ROOT, "README.md")] : [];
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    if (e.isDirectory()) found.push(...docFiles(join(dir, e.name)));
    else if (e.name.endsWith(".md")) found.push(join(dir, e.name));
  }
  return found;
}

/** The pictures the docs show (file name → the docs showing it), each one's path checked to lead to docs/img/shots. */
function shown() {
  const files = new Map();
  const wrong = [];
  for (const doc of docFiles()) {
    for (const m of readFileSync(doc, "utf8").matchAll(/["(\s]([^"()\s]*img\/shots\/([\w-]+\.png))/g)) {
      if (resolve(dirname(doc), m[1]) !== join(SHOTS, m[2])) wrong.push(`${relative(ROOT, doc)}: ${m[1]} doesn't lead to docs/img/shots`);
      files.set(m[2], [...(files.get(m[2]) ?? []), relative(ROOT, doc)]);
    }
  }
  return { files, wrong };
}

/** What's wrong between the list, the pictures and the docs: nothing, when they agree. */
function docsCheck() {
  const { files, wrong } = shown();
  const listed = new Set(shots.flatMap((s) => THEMES.map((t) => `${s.name}-${t}.png`)));
  const have = existsSync(SHOTS) ? readdirSync(SHOTS).filter((f) => f.endsWith(".png")) : [];
  for (const [f, docs] of files) if (!have.includes(f)) wrong.push(`${docs[0]} shows ${f}, which isn't in docs/img/shots`);
  for (const f of listed) if (!have.includes(f)) wrong.push(`${f} isn't in docs/img/shots: npm run shots`);
  for (const f of have) if (!listed.has(f)) wrong.push(`docs/img/shots/${f} is no shot of shots.config.mjs: remove it`);
  for (const s of shots) if (!THEMES.some((t) => files.has(`${s.name}-${t}.png`))) wrong.push(`no doc shows ${s.name}: show it, or take it off shots.config.mjs`);
  return wrong;
}

/** The interface's source as it is on disk (the tracked files it's built from, its tests left out): what the lock says the pictures were taken of. */
function webHash() {
  const r = spawnSync("git", ["ls-files", "-z", "web/src", "web/index.html", "web/package-lock.json"], { cwd: ROOT, encoding: "utf8" });
  if (r.status !== 0) fail(`git ls-files: ${r.stderr}`);
  const h = createHash("sha256");
  for (const f of r.stdout.split("\0").filter(Boolean).sort()) {
    if (/\.test\.tsx?$/.test(f) || !existsSync(join(ROOT, f))) continue;
    h.update(f).update("\0").update(readFileSync(join(ROOT, f))).update("\0");
  }
  return h.digest("hex");
}

const lock = existsSync(LOCK) ? JSON.parse(readFileSync(LOCK, "utf8")) : null;
/** The interface changed since the pictures were taken: each is written again. */
const stale = lock?.web !== webHash();
if (flag("--release")) {
  if (stale) fail("the interface (web/) changed since the docs' screenshots were taken: npm run shots, and commit docs/img/shots with the release.");
}

// ── before anything runs ────────────────────────────────────────────────────

const chrome = findChrome();
if (!chrome) fail("no Chrome found (install Google Chrome or Chromium, or set CHROME=/path/to/chrome).");
if (!existsSync(BIN)) fail(`no trunk-pro at ${BIN}: npm run build, then cargo build --release -p trunk-pro (or set TRUNK_PRO).`);
const dist = join(ROOT, "web/dist/index.html");
if (!existsSync(dist)) fail("no web/dist: npm run build first.");
const version = readFileSync(join(ROOT, "Cargo.toml"), "utf8").match(/^version = "(.+)"/m)[1];

const wanted = shots.filter((s) => !only || only.includes(s.name));
/** A scenario's captures that aren't there, by name. */
const missing = (sc) => [...new Set((sc.captures ?? []).filter(([c, f]) => !existsSync(join(CORPUS, "captures", c, f))).map(([c]) => c))];

// ── a scenario: its recorder, and its shots ─────────────────────────────────

let failed = 0;
let written = 0;
const skipped = [];
const kept = [];
const procs = new Set();
process.on("exit", () => procs.forEach((p) => p.kill()));

const chromeDir = mkdtempSync(join(tmpdir(), "trunk-pro-shots-chrome-"));
const chromium = await startChrome(chrome, chromeDir, ["--hide-scrollbars", "--font-render-hinting=none"]);
procs.add(chromium.proc);

/** The text of a page with the folders of this run named as a reader's would be. */
const tidy = (pairs) => `(() => {
  const pairs = ${JSON.stringify(pairs)};
  const fix = (s) => pairs.reduce((s, [a, b]) => s.split(a).join(b), s);
  const walk = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let n = walk.nextNode(); n; n = walk.nextNode()) if (pairs.some(([a]) => n.nodeValue.includes(a))) n.nodeValue = fix(n.nodeValue);
  for (const i of document.querySelectorAll("input, textarea")) if (pairs.some(([a]) => i.value.includes(a))) i.value = fix(i.value);
  for (const e of document.querySelectorAll("[title]")) if (pairs.some(([a]) => e.title.includes(a))) e.title = fix(e.title);
  // (A capture plays as a radio would be received: the state says what a reader's says.)
  // (Its text node is kept, so the interface still updates it; put back after the picture.)
  const pill = [...(document.querySelector(".pill")?.childNodes ?? [])].find((n) => n.nodeValue === "Replaying");
  if (pill) pill.nodeValue = "Recording";
  return true;
})()`;

/** Where `clip` is on the page: a selector, or the card with a heading. */
const rectOf = (clip) => `(() => {
  const clip = ${JSON.stringify(clip)};
  const el = typeof clip === "string"
    ? document.querySelector(clip)
    : [...document.querySelectorAll("h1, h2, h3, h4, .card-title, legend")].find((h) => h.textContent.trim() === clip.card)?.closest("section, .card, .panel, fieldset");
  if (!el) return null;
  const r = el.getBoundingClientRect();
  const pad = 12;
  return { x: Math.max(0, r.x - pad) + scrollX, y: Math.max(0, r.y - pad) + scrollY, width: r.width + 2 * pad, height: r.height + 2 * pad, found: el.tagName + "." + el.className };
})()`;

/** The share of two PNGs' pixels that differ (1 when their sizes do), compared in the page: no PNG library. */
const differ = (a, b) => `(async () => {
  const load = async (b64) => createImageBitmap(await (await fetch("data:image/png;base64," + b64)).blob());
  const [a, b] = await Promise.all([load(${JSON.stringify(a)}), load(${JSON.stringify(b)})]);
  if (a.width !== b.width || a.height !== b.height) return 1;
  const px = (i) => { const c = new OffscreenCanvas(i.width, i.height).getContext("2d"); c.drawImage(i, 0, 0); return c.getImageData(0, 0, i.width, i.height).data; };
  const [x, y] = [px(a), px(b)];
  let n = 0;
  for (let i = 0; i < x.length; i += 4) if (Math.abs(x[i] - y[i]) + Math.abs(x[i + 1] - y[i + 1]) + Math.abs(x[i + 2] - y[i + 2]) > 24) n++;
  return n / (x.length / 4);
})()`;

async function scenario(id, list) {
  const sc = scenarios[id];
  const dir = mkdtempSync(join(tmpdir(), `trunk-pro-shots-${id}-`));
  const config = join(dir, "config.json");
  const port = await freePort();
  const sources = (sc.captures ?? []).map(([c, f, centerHz]) => ({ type: "file", path: join(CORPUS, "captures", c, f), format: "cu8", realtime: true, centerHz, rateHz: 2400000 }));
  // (No update badge, no probes of the network: nothing of this machine's day in a picture.)
  writeFileSync(config, JSON.stringify({ server: { port }, monitor: { checkUpdates: false, networkChecks: false }, sources, recording: { captureDir: join(dir, "calls") }, ...sc.config }, null, 2));
  const tokens = {};
  for (const [user, access] of sc.users ?? []) {
    const r = spawnSync(BIN, ["access", "add", user, "--access", access, "--config", config], { encoding: "utf8" });
    if (r.status !== 0) throw new Error(`trunk-pro access add ${user}: ${r.stderr}`);
    tokens[user] = r.stdout.trim();
  }
  if (sc.users) spawnSync(BIN, ["access", "on", "--config", config]);

  const rec = await startRecorder(BIN, config, port);
  procs.add(rec.proc);
  const { origin } = rec;
  const page = await chromium.page();
  try {
    const served = await fetch(`${origin}/api/version`).then((r) => r.json());
    if (served.version !== version) throw new Error(`${BIN} is v${served.version}, the source v${version}: cargo build --release -p trunk-pro.`);
    if (!sc.users && (await fetch(`${origin}/`).then((r) => r.text())) !== readFileSync(dist, "utf8")) throw new Error(`${BIN} wasn't built with the interface in web/dist: cargo build --release -p trunk-pro (after npm run build).`);

    if (sc.talkgroups) {
      const names = JSON.parse(readFileSync(join(here, "shots/talkgroups.json"), "utf8"));
      const ws = new WebSocket(`ws://localhost:${port}/api/ws`);
      await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
      for (const [system, talkgroups] of Object.entries(names)) ws.send(JSON.stringify({ type: "setTalkgroups", system, talkgroups: talkgroups.map((t) => ({ priority: 1, preferredNac: 0, preferredSite: "", ignore: false, ...t })) }));
      await sleep(500);
      ws.close();
    }

    await page.send("Emulation.setTimezoneOverride", { timezoneId: "America/New_York" });
    await page.send("Emulation.setLocaleOverride", { locale: "en-US" });
    const size = (s) => page.send("Emulation.setDeviceMetricsOverride", { width: s.width ?? 1440, height: s.height ?? 900, deviceScaleFactor: 2, mobile: false });
    const theme = (t) => page.send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: t }, { name: "prefers-reduced-motion", value: "reduce" }] });
    const pairs = [[CORPUS, "/home/trunk/captures"], [dir, "/home/trunk/trunk-pro"]];
    const pill = () => page.evaluate(`document.querySelector(".pill")?.textContent ?? ""`);

    const signedOut = list.filter((s) => s.signedOut);
    const signedIn = list.filter((s) => !s.signedOut);
    const idle = signedIn.filter((s) => !s.live);
    const live = signedIn.filter((s) => s.live).sort((a, b) => (a.after ?? 35) - (b.after ?? 35));

    /** One shot, in each theme. */
    const take = async (s) => {
      try {
        await size(s);
        await theme(themes[0]);
        if (s.url.startsWith("/")) await page.goto(origin + s.url);
        else if (s.storage || !(await page.evaluate(`location.origin === ${JSON.stringify(origin)} && !!document.querySelector(".topbar")`).catch(() => false))) {
          // (An address differing only in its # loads nothing: the page is loaded again on it.)
          if (!(await page.evaluate(`location.origin === ${JSON.stringify(origin)}`).catch(() => false))) await page.goto(`${origin}/`);
          await page.evaluate(`(() => { for (const [k, v] of Object.entries(${JSON.stringify(s.storage ?? {})})) localStorage.setItem(k, v); location.hash = ${JSON.stringify(s.url)}; return true; })()`);
          const loaded = page.event("Page.loadEventFired");
          await page.send("Page.reload");
          await loaded;
        } else await page.evaluate(`location.hash = ${JSON.stringify(s.url)}; true`);
        if (!s.url.startsWith("/")) await page.waitFor(`!!document.querySelector(".topbar") && document.querySelector(".pill")?.textContent !== "Disconnected"`, "the dashboard, connected");
        for (const step of s.prepare ?? []) {
          if (step.tab) await page.waitFor(`(() => { const b = [...document.querySelectorAll("[role=tab]")].find((b) => b.textContent.trim().startsWith(${JSON.stringify(step.tab)})); b?.click(); return !!b; })()`, `the ${step.tab} tab`);
          else if (step.click) await page.waitFor(`(() => { const b = [...document.querySelectorAll("button, a")].find((b) => b.textContent.trim() === ${JSON.stringify(step.click)}); b?.click(); return !!b; })()`, `the ${step.click} button`);
          else if (step.wait) await page.waitFor(step.wait, step.wait);
          else if (step.eval) await page.evaluate(step.eval);
        }
        if (s.ready) await page.waitFor(s.ready, `${s.name} to be ready (${s.ready})`, 20_000);
        // (Nothing focused, nothing under the pointer.)
        await page.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: 0, y: 0 });
        await page.evaluate(`document.activeElement?.blur(), true`);
        await page.evaluate(`(() => { const st = document.createElement("style"); st.textContent = "*,*::before,*::after{caret-color:transparent!important;transition:none!important;animation:none!important}"; document.head.append(st); return true; })()`);
        for (const [i, t] of themes.entries()) {
          await theme(t);
          await sleep(i === 0 ? (s.settle ?? 300) : Math.min(s.settle ?? 300, 1500));
          const bad = await page.evaluate(`document.querySelector(".banner.bad")?.textContent ?? null`);
          if (bad) throw new Error(`the page says: ${bad}`);
          await page.evaluate(tidy(pairs));
          for (const [sel, text] of Object.entries(s.redact ?? {})) await page.evaluate(`(() => { for (const e of document.querySelectorAll(${JSON.stringify(sel)})) e.textContent = ${JSON.stringify(text)}; return true; })()`);
          let clip;
          if (s.clip) {
            const r = await page.evaluate(rectOf(s.clip));
            if (!r) throw new Error(`nothing on the page for clip ${JSON.stringify(s.clip)}`);
            clip = { x: r.x, y: r.y, width: r.width, height: r.height, scale: 1 };
            if (process.env.SHOTS_DEBUG) console.log(`# ${s.name}: clip ${JSON.stringify(r)}`);
          }
          await page.evaluate(`document.fonts.ready.then(() => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)))).then(() => true)`);
          const { data } = await page.send("Page.captureScreenshot", { format: "png", ...(clip ? { clip, captureBeyondViewport: true } : {}) });
          await page.evaluate(`(() => { const n = [...(document.querySelector(".pill")?.childNodes ?? [])].find((n) => n.nodeValue === "Recording"); if (n) n.nodeValue = "Replaying"; return true; })()`);
          const file = join(out, `${s.name}-${t}.png`);
          // (With the interface as it was when they were taken, rewritten only when it shows something else: a clock's digits aren't a change.)
          const same = !stale && existsSync(file) && (await page.evaluate(differ(readFileSync(file).toString("base64"), data))) < 0.005;
          if (!same) {
            writeFileSync(file, Buffer.from(data, "base64"));
            written++;
          }
          console.log(`ok - ${s.name}-${t}${same ? " (unchanged)" : ""}`);
        }
      } catch (e) {
        failed++;
        console.log(`not ok - ${s.name}\n  ${String(e?.message ?? e).split("\n").join("\n  ")}`);
      }
    };

    for (const s of signedOut) await take(s);
    if (sc.users && signedIn.length) {
      await page.goto(`${origin}/signin`);
      await page.waitFor(`document.title === "Sign in" && !!document.getElementById("t")`, "the sign-in page");
      await page.evaluate(`document.getElementById("t").value = ${JSON.stringify(tokens[sc.users[0][0]])}; document.getElementById("f").requestSubmit(); true`);
      await page.waitFor(`!!document.querySelector(".topbar")`, "the dashboard, signed in");
    }
    for (const s of idle) await take(s);

    // The replay: each shot `after` seconds into a pass, a new pass when one's too far gone.
    let began = 0;
    for (const s of live) {
      const cost = ((s.settle ?? 300) + 4000) / 1000;
      const at = () => (Date.now() - began) / 1000;
      if (!began || at() + cost > LAST_S || (await pill()) !== "Replaying") {
        if (!(await page.evaluate(`!!document.querySelector(".topbar")`).catch(() => false))) await page.goto(`${origin}/#/overview`);
        if (began) await page.waitFor(`document.querySelector(".pill")?.textContent === "Stopped"`, "the replay to end", (REPLAY_S + 30) * 1000);
        const start = `[...document.querySelectorAll(".topbar button")].find((b) => b.textContent.trim() === "Start")`;
        await page.waitFor(`!!${start}`, "the Start button");
        await sleep(500);
        const problem = await page.evaluate(`${start}.disabled ? ${start}.title || "Start is off" : null`);
        if (problem) throw new Error(`the recorder can't start: ${problem}`);
        await page.evaluate(`${start}.click(), true`);
        await page.waitFor(`document.querySelector(".pill")?.textContent === "Replaying"`, "the replay to start", 30_000);
        began = Date.now();
      }
      const wait = (s.after ?? 35) - at();
      if (wait > 0) await sleep(wait * 1000);
      await take(s);
    }
  } finally {
    if (failed) console.log(`\n# ${id}: trunk-pro's log (last lines):\n${logTail(rec.log(), 15)}`);
    await page.send("Page.close").catch(() => {});
    rec.proc.kill();
    await ended(rec.proc);
    procs.delete(rec.proc);
    if (flag("--keep")) kept.push(dir);
    else rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }
}

mkdirSync(out, { recursive: true });
for (const id of Object.keys(scenarios)) {
  const list = wanted.filter((s) => s.scenario === id);
  if (!list.length) continue;
  const gone = missing(scenarios[id]);
  if (gone.length) {
    // (CI without the corpus still checks the rest; taking the pictures needs them all.)
    const why = `the ${gone.join(" and ")} capture${gone.length > 1 ? "s aren't" : " isn't"} in ${relative(ROOT, CORPUS)} (trunk-pro tool corpus fetch --only ${gone.join(",")})`;
    if (!check) fail(why);
    skipped.push(...list.map((s) => s.name));
    console.log(`# skipped ${list.length} shots of "${id}": ${why}`);
    continue;
  }
  try {
    await scenario(id, list);
  } catch (e) {
    failed++;
    console.log(`not ok - ${id}\n  ${String(e?.message ?? e).split("\n").join("\n  ")}`);
  }
}

chromium.close();
chromium.proc.kill();
await ended(chromium.proc);
// (Chrome's helpers may still be writing its profile: a temporary folder left behind is no failure.)
if (!flag("--keep")) try { rmSync(chromeDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 }); } catch (e) { console.log(`# left ${chromeDir}: ${e.code ?? e}`); }
for (const d of kept) console.log(`kept: ${d}`);

// ── the docs, and the lock ──────────────────────────────────────────────────

if (!failed && !check && !only && themes.length === THEMES.length && out === SHOTS) {
  writeFileSync(LOCK, `${JSON.stringify({ version, web: webHash() }, null, 2)}\n`);
}
const wrong = only ? [] : docsCheck();
for (const w of wrong) console.log(`not ok - ${w}`);
failed += wrong.length;

// (A check's pictures are kept only when one failed, or with --keep.)
const temporary = check && !value("--out");
if (temporary && !failed && !flag("--keep")) rmSync(out, { recursive: true, force: true });
console.log(failed ? `\n${failed} failed${temporary ? ` (rendered into ${out})` : ""}` : `\nall passed${check ? "" : `: ${written} written`}${skipped.length ? `, ${skipped.length} skipped` : ""}`);
process.exit(failed ? 1 : 0);
