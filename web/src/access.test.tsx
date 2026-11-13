// Signing in, as the dashboard shows it: a View connection (listen) sees
// every page but those that change the recorder, on the config it is given
// (no plugins, keys, store URL or users), and none of the controls; an
// Admin one (control) sees them all. Setup's Access section lists the users,
// shows a new one's token secret once, and turns signing in on only once
// this browser has signed in as an Admin. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { App } from "./App.tsx";
import { AccessSection, enableWith } from "./Access.tsx";
import { defaultConfig, newSystem } from "./config.ts";
import { mayEdit, passkeyName, setSetupTab, setView, snapshot, transport, type View } from "./controller.ts";
import { IgnoreToggle, NameButton } from "./dash/Radio.tsx";
import { InterfacesPage } from "./Interfaces.tsx";
import type { ApiAccess, Config, EngineStatus, FromRecorder, Phase, TgRow, ToRecorder } from "./protocol.ts";

const text = (html: string) => html.replace(/<!-- -->/g, "").replace(/<[^>]+>/g, " ").replace(/&#x27;/g, "'").replace(/\s+/g, " ");
const nav = (html: string) => text(/<nav class="rail-nav"[\s\S]*?<\/nav>/.exec(html)?.[0] ?? "");

/** A config of one system, signing in on with an Admin user and a View one. */
function full(): Config {
  const c = defaultConfig();
  c.systems.push(newSystem(c, { shortName: "dcfd", controlChannelsHz: [851_000_000] }));
  c.plugins = { openmhz: { enabled: true, settings: { apiKey: "secret" } } };
  c.keys = { fleet: { file: "/keys.ekc", passwordFile: "/pw" } };
  c.storage = { backend: "postgres", url: "postgres://u:p@db/trp", retentionDays: 120, commitSeconds: 300 };
  c.server.auth = { enabled: true, users: { luke: { access: "control", token: { hash: "aa", created: 1_763_000_000 } }, kiosk: { access: "listen", token: { hash: "bb", created: 1_763_100_000 } } } };
  return c;
}

/** What a View connection is given: `full()` without what only control sees. */
function redacted(): Config {
  const c = full();
  delete c.plugins;
  delete c.keys;
  delete c.storage!.url;
  delete c.server.auth;
  for (const s of c.systems) delete s.plugins;
  return c;
}

/** The recorder's greeting to a connection with `access` (signed in as `user`), in `phase`. */
function hello(c: Config, access: ApiAccess, user?: string, phase: Phase = "idle"): void {
  const greeting: FromRecorder[] = [
    { type: "state", phase, error: null, ended: false, restartNeeded: false },
    { type: "config", config: c, version: 1 },
    { type: "history", entries: [] },
    { type: "hello", version: "test", platform: "test", api: 1, access, ...(user ? { user } : {}), surveyBands: [], capabilities: ["service", "drivers", "plugins", "interfaces", "host", "keys", "history"] },
  ];
  transport.onConnection(true);
  for (const m of greeting) transport.onMessage(m);
  transport.onMessage({ type: "status", status: { systems: [], recording: 0 } as unknown as EngineStatus, sources: [], load: 0, calls: [] });
}

const ROUTES: [View, ...string[]][] = [["overview"], ["listen"], ["rf"], ["decode"], ["decode", "dcfd"], ["radio"], ["radio", "dcfd"], ["radio", "dcfd", "tg", "101"], ["radio", "dcfd", "unit", "5"], ["plugin", "openmhz"], ["plugins"], ["interfaces"], ["platform"], ["platform", "storage"], ["setup"]];
const row = { tg: 101, alphaTag: "Fire Dispatch", ignore: false } as TgRow;
const sys = { name: "dcfd", kind: "p25" as const, status: null, sites: [] };

test("View: every page renders on the config it's given, without setup, plugins, interfaces, Start or editors", () => {
  hello(redacted(), "listen", "kiosk");
  assert.equal(mayEdit(), false);
  for (const [view, ...path] of ROUTES) {
    setView(view, ...path);
    const html = renderToString(<App />);
    const page = text(html);
    assert.doesNotMatch(nav(html), /Setup|Plugins|Interfaces/, `${view}: no editing pages in the navigation`);
    assert.doesNotMatch(html, />(Start|Stop|Quit|Restart|Setup|Setup guide|Open setup|Setup → Data|Signals in Setup|Review in Setup)</, `${view}: nothing that runs the recorder`);
    if (["plugin", "plugins", "interfaces", "setup"].includes(view)) assert.match(html, /<h1>Overview<\/h1>/, `${view}: the Overview in its place`);
    assert.doesNotMatch(page, /Allowed origins|Hosts to check|Record the control channel/, view);
  }
  assert.equal(renderToString(<NameButton named open={false} onClick={() => {}} />), "", "no names editor");
  assert.equal(renderToString(<IgnoreToggle sys={sys} r={row} />), "", "no Ignore");
});

test("View: signed in, the user's name and Sign out (not when the proxy in front signed them in); not signed in, with signing in on, Sign in", async () => {
  hello(redacted(), "listen", "kiosk");
  setView("overview");
  assert.match(text(renderToString(<App />)), /kiosk Sign out/);
  const fetch0 = globalThis.fetch;
  const session = (auth: boolean, user: string | null = null, proxy = false) =>
    (globalThis.fetch = (async (url: string) => {
      assert.equal(url, "/api/session");
      return new Response(JSON.stringify({ auth, access: "listen", user, proxy }), { status: 200 });
    }) as typeof fetch);
  try {
    // Anonymous View: the session says signing in is on (the config it's given doesn't).
    session(true);
    hello(redacted(), "listen");
    await new Promise((r) => setTimeout(r, 20));
    assert.equal(snapshot().signingIn, true);
    const html = renderToString(<App />);
    assert.match(html, /<a class="btn ghost small" href="\/signin">Sign in<\/a>/);
    assert.doesNotMatch(text(html), /Sign out/);
    // Signed in by the proxy: their name, and signing out is the proxy's.
    session(true, "kiosk", true);
    hello(redacted(), "listen", "kiosk");
    await new Promise((r) => setTimeout(r, 20));
    assert.equal(snapshot().proxied, true);
    const page = text(renderToString(<App />));
    assert.match(page, /kiosk/);
    assert.doesNotMatch(page, /Sign (out|in)/);
    session(false);
    hello(full(), "control");
    await new Promise((r) => setTimeout(r, 20));
    assert.doesNotMatch(text(renderToString(<App />)), /Sign (out|in)/, "signing in off: nothing");
  } finally {
    globalThis.fetch = fetch0;
  }
});

test("Admin: the editing pages, Start and the editors are all there", () => {
  hello(full(), "control", "luke");
  assert.equal(mayEdit(), true);
  setView("overview");
  const html = renderToString(<App />);
  assert.match(nav(html), /Plugins .*Interfaces .*Setup/);
  assert.match(html, />Start</);
  assert.match(html, />Quit</);
  setView("setup");
  assert.match(renderToString(<App />), /<h1>Setup<\/h1>/);
  assert.match(renderToString(<NameButton named open={false} onClick={() => {}} />), /Edit names/);
  assert.match(renderToString(<IgnoreToggle sys={sys} r={row} />), /Ignore/);
});

test("Access: the users, each's role and when their token was made; the last Admin can't be removed while on", () => {
  hello(full(), "control", "luke");
  setSetupTab("recording");
  setView("setup");
  const page = text(renderToString(<App />));
  assert.match(page, /Access Tokens, passkeys/);
  const html = renderToString(<AccessSection c={full()} />);
  assert.match(text(html), /Name Role Token Passkeys luke .* kiosk/);
  assert.match(html, /aria-label="Role of luke" disabled=""/, "the only Admin stays Admin");
  assert.doesNotMatch(html, /aria-label="Remove luke"/);
  assert.match(html, /aria-label="Remove kiosk"/);
  assert.match(html, /role="switch" checked=""/);
  const off = full();
  off.server.auth!.enabled = false;
  assert.match(renderToString(<AccessSection c={off} />), /aria-label="Remove luke"/, "off: any can go");
  assert.match(text(renderToString(<AccessSection c={{ ...off, server: { ...off.server, auth: undefined } }} />)), /No users/);
  // An Admin with nothing to sign in with can't turn it off again: the one who can stays.
  const both = full();
  both.server.auth!.users!.ann = { access: "control" };
  const two = renderToString(<AccessSection c={both} />);
  assert.match(two, /aria-label="Role of luke" disabled=""/);
  assert.match(two, /aria-label="Remove ann"/);
  assert.match(text(two), /ann .*—/, "no token: no date");
});

/** `hello` as `user`, the session saying whether passkeys work here. */
async function helloWithPasskeys(c: Config, user: string, passkeys: boolean): Promise<void> {
  const fetch0 = globalThis.fetch;
  globalThis.fetch = (async () => new Response(JSON.stringify({ auth: true, access: "control", user, passkeys }), { status: 200 })) as typeof fetch;
  try {
    hello(c, "control", user);
    await new Promise((r) => setTimeout(r, 20));
  } finally {
    globalThis.fetch = fetch0;
  }
}

test("Access: each user's passkeys by name, to remove; Add passkey for this browser's user alone, where passkeys work", async () => {
  const c = full();
  c.server.auth!.users!.kiosk.passkeys = [{ name: "Wall tablet", created: 1_763_200_000, id: "AQID", key: {} }];
  c.server.auth!.users!.luke.passkeys = [{ name: "Mac Safari", created: 1_763_200_000, id: "BAUG", key: {} }, { name: "iPhone", created: 1_763_300_000, id: "BwgJ", key: {} }];
  await helloWithPasskeys(c, "luke", true);
  assert.equal(snapshot().passkeys, true);
  const html = renderToString(<AccessSection c={c} />);
  assert.match(text(html), /luke .*Mac Safari .*iPhone .*Add passkey .*kiosk .*Wall tablet/);
  assert.equal(html.match(/>Add passkey</g)?.length, 1, "only for luke, signed in here");
  for (const l of ["Remove luke's passkey Mac Safari", "Remove luke's passkey iPhone", "Remove kiosk's passkey Wall tablet", "Remove luke's token", "Remove kiosk's token"]) assert.match(html, new RegExp(`aria-label="${l.replace("'", "&#x27;")}"`), l);
  // Passkeys don't work where this page is (an IP address, plain HTTP on a name): no Add passkey.
  await helloWithPasskeys(c, "luke", false);
  assert.doesNotMatch(renderToString(<AccessSection c={c} />), />Add passkey</);
  // The last Admin keeps a way in: with a passkey and no token, that passkey stays; with one of each, neither's last.
  const one = full();
  delete one.server.auth!.users!.luke.token;
  one.server.auth!.users!.luke.passkeys = [{ name: "iPhone", created: 1, id: "AQID", key: {} }];
  const last = renderToString(<AccessSection c={one} />);
  assert.doesNotMatch(last, /aria-label="Remove luke&#x27;s passkey iPhone"/);
  assert.match(last, /aria-label="Role of luke" disabled=""/, "a passkey alone makes them an Admin who can sign in");
  assert.doesNotMatch(renderToString(<AccessSection c={full()} />), /aria-label="Remove luke&#x27;s token"/, "a token alone: it stays");
});

test("Access: the proxy in front, set up with its addresses and header; then From, Header, Others and how many names are mapped", () => {
  const c = full();
  const none = text(renderToString(<AccessSection c={c} />));
  assert.match(none, /Proxy None/);
  assert.match(renderToString(<AccessSection c={c} />), />Set up</);
  c.server.auth!.proxy = { from: ["127.0.0.1", "10.0.0.0/8"], header: "X-Forwarded-User", users: { "luke@example.com": "luke" }, other: "listen" };
  const html = renderToString(<AccessSection c={c} />);
  assert.match(text(html), /Proxy Signs people in/);
  assert.match(html, /aria-label="Proxy addresses"[^>]*value="127.0.0.1, 10.0.0.0\/8"/);
  assert.match(html, /aria-label="Proxy header"[^>]*value="X-Forwarded-User"/);
  assert.match(html, /role="radio" aria-checked="true" class="on">View</);
  assert.match(text(html), /1 mapped/);
  assert.match(html, />Remove</);
  assert.doesNotMatch(html, />Set up</);
});

test("Access: a passkey's default name is the system and browser", () => {
  assert.equal(passkeyName("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Safari/605.1.15"), "Mac Safari");
  assert.equal(passkeyName("Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.1 Mobile/15E148 Safari/604.1"), "iPhone Safari");
  assert.equal(passkeyName("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0"), "Windows Edge");
  assert.equal(passkeyName("Mozilla/5.0 (Linux; Android 15) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile Safari/537.36"), "Android Chrome");
  assert.equal(passkeyName("Mozilla/5.0 (X11; Linux x86_64; rv:133.0) Gecko/20100101 Firefox/133.0"), "Linux Firefox");
  assert.equal(passkeyName(""), "Passkey");
});

test("Access: a user just made shows their token's secret once, to copy", () => {
  const made = { type: "tokenCreated" as const, id: 1, name: "wall", secret: "trp_0123abcd" };
  const page = text(renderToString(<AccessSection c={full()} made={{ name: made.name, access: "listen", secret: made.secret }} />));
  assert.match(page, /wall View trp_0123abcd Copy Shown once/);
  assert.doesNotMatch(text(renderToString(<AccessSection c={full()} />)), /trp_/, "nowhere else");
});

test("Access: signing in turns on only after this browser signed in as an Admin", async () => {
  const off = full();
  off.server.auth = { enabled: false };
  hello(off, "control");
  const log: string[] = [];
  const sent: ToRecorder[] = [];
  const send = transport.send;
  const fetch0 = globalThis.fetch;
  transport.send = (m) => {
    sent.push(m);
    if (m.type === "setConfig") log.push(`setConfig enabled=${m.config.server.auth?.enabled}`);
  };
  // The recorder's answer to POST /api/login: who signed in, by the secret.
  const users: Record<string, { access: ApiAccess; user: string }> = { trp_luke: { access: "control", user: "luke" }, trp_kiosk: { access: "listen", user: "kiosk" } };
  globalThis.fetch = (async (url: string, init?: RequestInit) => {
    const who = url === "/api/login" ? users[JSON.parse(String(init?.body)).token] : undefined;
    log.push(`${url} ${url === "/api/login" ? (who ? 200 : 401) : 204}`);
    return who ? new Response(JSON.stringify(who), { status: 200 }) : new Response(null, { status: url === "/api/login" ? 401 : 204 });
  }) as typeof fetch;
  try {
    assert.equal(await enableWith("trp_nope"), "Not a token here");
    assert.equal(await enableWith("trp_kiosk"), "Not an Admin token");
    await new Promise((r) => setTimeout(r, 400));
    assert.deepEqual(log, ["/api/login 401", "/api/login 200", "/api/logout 204"], "refused: nothing saved, and a View user's cookie out again");
    // The user is made (createToken), and the config with them comes after.
    const on = enableWith("trp_luke");
    setTimeout(() => transport.onMessage({ type: "config", config: { ...off, server: { ...off.server, auth: { enabled: false, users: { luke: { access: "control", token: { hash: "aa", created: 1 } } } } } }, version: 2 }), 50);
    assert.equal(await on, null);
    await new Promise((r) => setTimeout(r, 400));
    assert.deepEqual(log.slice(3), ["/api/login 200", "setConfig enabled=true"]);
    const saved = sent.findLast((m) => m.type === "setConfig");
    assert.ok(saved?.type === "setConfig" && saved.config.server.auth?.users?.luke, "the user kept");
    transport.onMessage({ type: "configSaved", version: 3 });
  } finally {
    transport.send = send;
    globalThis.fetch = fetch0;
  }
});

test("Interfaces: with signing in on, an allowed origin needs an Admin's token too", () => {
  hello(full(), "control", "luke");
  transport.onMessage({ type: "interfaces", interfaces: [], api: 1 });
  assert.match(text(renderToString(<InterfacesPage />)), /Allowed origins \(control, with an Admin token\)/);
  const off = full();
  off.server.auth!.enabled = false;
  hello(off, "control");
  assert.match(text(renderToString(<InterfacesPage />)), /Allowed origins \(with control\)/);
  assert.equal(snapshot().user, null);
});

test("Interfaces: HTTPS's certificate and key, and the host name, beside the ports", () => {
  const c = full();
  hello(c, "control", "luke");
  transport.onMessage({ type: "interfaces", interfaces: [], api: 1 });
  assert.match(text(renderToString(<InterfacesPage />)), /HTTPS every port .* Off/);
  c.server.tls = { cert: "/etc/ssl/radiobox.crt", key: "/etc/ssl/radiobox.key" };
  hello(c, "control", "luke");
  const html = renderToString(<InterfacesPage />);
  assert.match(html, /aria-label="Certificate" [^>]*value="\/etc\/ssl\/radiobox.crt"/);
  assert.match(html, /aria-label="Key" [^>]*value="\/etc\/ssl\/radiobox.key"/);
  assert.match(text(html), /From next restart/, "served over plain HTTP until then");
  assert.match(text(html), /Host name passkeys .* None/);
  c.server.hostname = "radiobox.tail1234.ts.net";
  hello(c, "control", "luke");
  assert.match(renderToString(<InterfacesPage />), /aria-label="Host name" [^>]*value="radiobox.tail1234.ts.net"/);
});
