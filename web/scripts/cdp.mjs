// The real trunk-pro binary on a config of its own, and headless Chrome driven
// over the Chrome DevTools Protocol (no library: Node's WebSocket). Shared by
// auth-browser.mjs (signing in, tested) and shots.mjs (the docs' screenshots).

import { spawn, spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { createServer } from "node:net";
import { join } from "node:path";

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export const freePort = () => new Promise((res) => { const s = createServer().listen(0, "127.0.0.1", () => { const p = s.address().port; s.close(() => res(p)); }); });

/** CHROME, or Google Chrome or Chromium where they're installed; null when there's none. */
export function findChrome() {
  if (process.env.CHROME) return existsSync(process.env.CHROME) ? process.env.CHROME : null;
  for (const p of ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/Applications/Chromium.app/Contents/MacOS/Chromium", "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe"]) if (existsSync(p)) return p;
  for (const name of ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"]) {
    const r = spawnSync("which", [name], { encoding: "utf8" });
    if (r.status === 0 && r.stdout.trim()) return r.stdout.trim();
  }
  return null;
}

/** (Waited for before a temporary folder goes: Chrome writes its profile as it quits.) */
export const ended = (p) => (p.exitCode !== null || p.signalCode !== null ? Promise.resolve() : new Promise((r) => { p.once("exit", r); setTimeout(r, 5000); }));

/** trunk-pro on `config` and `port`, once it answers: `{ proc, origin, log() }`. */
export async function startRecorder(bin, config, port, args = []) {
  let log = "";
  const proc = spawn(bin, ["--config", config, "--port", String(port), "--no-open", ...args], { stdio: ["ignore", "pipe", "pipe"] });
  proc.stdout.on("data", (d) => (log += d));
  proc.stderr.on("data", (d) => (log += d));
  const origin = `http://localhost:${port}`;
  for (let t = Date.now(); ; await sleep(100)) {
    if (await fetch(`${origin}/api/version`).then((r) => r.ok, () => false)) break;
    if (Date.now() - t > 30_000 || proc.exitCode !== null) {
      proc.kill();
      throw new Error(`trunk-pro didn't start:\n${log}`);
    }
  }
  return { proc, origin, log: () => log };
}

/** The log's last lines, the radios' own left out. */
export const logTail = (log, n = 30) => log.split("\n").filter((l) => !/\(info\)\s+(Opening device|starting event loop|event loop thread)/.test(l)).slice(-n).join("\n");

/**
 * Headless Chrome with its profile in `profile`: `{ proc, send, event, listeners, page(), close() }`.
 * `send(method, params, sessionId)` is one DevTools command; `page()` opens a tab.
 */
export async function startChrome(chrome, profile, args = []) {
  const proc = spawn(chrome, ["--headless=new", "--remote-debugging-port=0", `--user-data-dir=${profile}`, "--no-first-run", "--no-default-browser-check", "--disable-gpu", "--disable-extensions", ...(process.getuid?.() === 0 || process.env.CI ? ["--no-sandbox"] : []), ...args, "about:blank"], { stdio: "ignore" });
  let wsUrl = null;
  for (let t = Date.now(); !wsUrl; await sleep(100)) {
    const f = join(profile, "DevToolsActivePort");
    if (existsSync(f)) {
      const [p, path] = readFileSync(f, "utf8").split("\n");
      if (p && path) wsUrl = `ws://127.0.0.1:${p}${path}`;
    }
    if (Date.now() - t > 20_000) {
      proc.kill();
      throw new Error("Chrome didn't open its DevTools port");
    }
  }
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

  /** A tab: `sid` its session, and the helpers on it. */
  const page = async () => {
    const { targetId } = await send("Target.createTarget", { url: "about:blank" });
    const { sessionId: sid } = await send("Target.attachToTarget", { targetId, flatten: true });
    await send("Page.enable", {}, sid);
    await send("Runtime.enable", {}, sid);
    await send("Network.enable", {}, sid);
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
    /** The button or link saying exactly `text`, clicked. */
    const click = (text, scope = "document") => evaluate(`(() => { const b = [...${scope}.querySelectorAll("button, a")].find((b) => b.textContent.trim() === ${JSON.stringify(text)}); if (!b) throw new Error("no " + ${JSON.stringify(text)}); b.click(); return true; })()`);
    return { sid, send: (method, params = {}) => send(method, params, sid), event: (method, ms) => event(method, sid, ms), evaluate, goto, waitFor, click };
  };

  return { proc, send, event, listeners, page, close: () => ws.close() };
}
