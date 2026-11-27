// The Plugins page as rendered (react-dom/server): each plugin can be taken
// again from where it came, and uninstalled. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { defaultConfig, newSystem } from "./config.ts";
import { transport } from "./controller.ts";
import { PluginSetupPanel, PluginsPage, SystemPluginSettings } from "./Plugins.tsx";
import type { Config, PluginInfo, PluginManifest } from "./protocol.ts";

const plugin = (id: string, more: Partial<PluginInfo>): PluginInfo => ({
  id, path: `/plugins/${id}/${id}`, custom: false, problem: null,
  manifest: { id, name: id, version: "1.0.0", description: "", api: 1, subscribe: [], audio_formats: [] },
  runtime: { state: "off", message: "", ok: 0, skipped: 0, failed: 0, lastFailure: "", log: [] },
  ...more,
});

test("Plugins: a build of the user's own and one from GitHub can be reinstalled; each is uninstalled", () => {
  transport.onMessage({ type: "config", config: defaultConfig(), version: 1 });
  transport.onMessage({ type: "plugins", encoderFound: null, plugins: [plugin("mine", { custom: true, path: "/src/mine" }), plugin("theirs", { unlistedFrom: "someone/theirs" })] });
  const html = renderToString(<PluginsPage />);
  assert.equal((html.match(/>Reinstall</g) ?? []).length, 2, html);
  assert.equal((html.match(/>Uninstall</g) ?? []).length, 2);
  assert.ok(!html.includes(">Remove<"));
});

test("Plugins: a plugin's notices show under its status, the one its status already says once", () => {
  const c = defaultConfig();
  c.plugins = { up: { enabled: true, settings: {} } };
  transport.onMessage({ type: "config", config: c, version: 1 });
  const notices = [{ state: "warning" as const, message: "System 77 is uploading 2 sites." }, { state: "error" as const, message: "Key refused." }];
  const runtime = { state: "error" as const, message: "Key refused.", ok: 0, skipped: 0, failed: 0, lastFailure: "", log: [], notices };
  transport.onMessage({ type: "plugins", encoderFound: null, plugins: [plugin("up", { runtime })] });
  const page = renderToString(<PluginsPage />);
  assert.match(page, /plugin-notice warn-text">System 77 is uploading 2 sites\.</);
  assert.equal((page.match(/Key refused\./g) ?? []).length, 1, page);
});

/** Rendered, without React's text markers. */
const html = (n: React.ReactElement) => renderToString(n).replace(/<!-- -->/g, "");

/** Two systems (ids 1 and 2) and an uploader with a server and an API key, of which a system may set the server again. */
function uploaderSetup(more: Partial<PluginManifest> = {}): Config {
  const c = defaultConfig();
  c.systems.push({ ...newSystem(c, { shortName: "east" }), id: 1 }, { ...newSystem(c, { shortName: "west" }), id: 2 });
  c.plugins = { up: { enabled: true, settings: { server: "https://a", apiKey: "k" } } };
  const config = { type: "object", properties: { server: { type: "string", title: "Server" }, apiKey: { type: "string", title: "API key" } } };
  transport.onMessage({ type: "config", config: c, version: 1 });
  transport.onMessage({ type: "plugins", encoderFound: null, plugins: [plugin("up", { manifest: { id: "up", name: "Up", version: "1.0.0", description: "", api: 1, subscribe: ["call.concluded"], audio_formats: [], config, ...more } as PluginManifest })] });
  return c;
}

test("Setup → Plugins: a plugin is turned on and off system by system", () => {
  const c = uploaderSetup();
  c.plugins!.up.systemsOff = [2];
  transport.onMessage({ type: "config", config: c, version: 2 });
  const panel = html(<PluginSetupPanel />);
  assert.match(panel, /aria-label="Systems Up is on for"/);
  assert.match(panel, /aria-pressed="true"[^>]*>east</);
  assert.match(panel, /aria-pressed="false"[^>]*>west</);
});

test("A system's card offers only the settings a plugin lets a system set again, and not a plugin off for it", () => {
  uploaderSetup({ system_overrides: ["server"] });
  const card = html(<SystemPluginSettings system={0} />);
  assert.match(card, /Up shared settings/);
  assert.match(card, /Server/);
  assert.doesNotMatch(card, /API key/, "not offered");
  // None offered (the default), and nothing of its own per system: not on the card at all.
  uploaderSetup();
  assert.equal(renderToString(<SystemPluginSettings system={0} />), "");
  // Off for the system: not on its card.
  const c = uploaderSetup({ system_overrides: ["server"] });
  c.plugins!.up.systemsOff = [1];
  transport.onMessage({ type: "config", config: c, version: 2 });
  assert.equal(renderToString(<SystemPluginSettings system={0} />), "");
  assert.match(html(<SystemPluginSettings system={1} />), /Up shared settings/);
});

test("Settings a system sets again that the plugin doesn't offer are shown as ignored, to remove", () => {
  const c = uploaderSetup({ system_overrides: ["server"] });
  c.systems[1].plugins = { up: { override: { apiKey: "other", server: "https://b" } } };
  transport.onMessage({ type: "config", config: c, version: 2 });
  const panel = html(<PluginSetupPanel />);
  assert.match(panel, /Ignored overrides<\/span> <span class="muted">west<\/span>/);
  assert.match(panel, />Remove</);
});

test("A system with sites: each is turned on and off, and sets again what the plugin offers it", () => {
  const c = defaultConfig();
  const sys = newSystem(c, { shortName: "east", sites: [{ name: "north", controlChannelsHz: [851e6] }, { name: "south", controlChannelsHz: [852e6] }] });
  sys.id = 1;
  sys.sites[0].id = 7;
  sys.sites[1].id = 8;
  c.systems.push(sys);
  c.plugins = { up: { enabled: true, settings: {}, sitesOff: [8] } };
  const system_config = { type: "object", properties: { apiKey: { type: "string", title: "API key" }, systemId: { type: "integer", title: "System ID" } } };
  transport.onMessage({ type: "config", config: c, version: 1 });
  const manifest = { id: "up", name: "Up", version: "1.0.0", description: "", api: 1, subscribe: ["call.concluded"], audio_formats: [], system_config, site_overrides: ["systemId"] } as PluginManifest;
  transport.onMessage({ type: "plugins", encoderFound: null, plugins: [plugin("up", { manifest })] });
  const card = html(<SystemPluginSettings system={0} />);
  assert.match(card, /aria-label="Sites Up is on for"/);
  assert.match(card, /aria-pressed="true"[^>]*>north</);
  assert.match(card, /aria-pressed="false"[^>]*>south</);
  // North's own row offers its System ID, not its API key; south, off, has none.
  assert.match(card, /<summary>north<span class="muted"> · as east<\/span><\/summary>/);
  assert.match(card, /id="sp-up-east-site7-systemId"/);
  assert.doesNotMatch(card, /sp-up-east-site7-apiKey/);
  assert.doesNotMatch(card, /<summary>south/);
});

test("Setup → Plugins: each plugin is a card that folds, its switch beside the head, with Expand all for several", () => {
  const c = uploaderSetup();
  c.plugins!.other = { enabled: false };
  transport.onMessage({ type: "config", config: c, version: 2 });
  transport.onMessage({ type: "plugins", encoderFound: null, plugins: [plugin("up", {}), plugin("other", {})] });
  const panel = html(<PluginSetupPanel />);
  assert.match(panel, />Expand all</);
  // Two cards, folded (only a list of one starts open), each found again by its import to-do id.
  assert.equal((panel.match(/class="set-card-toggle" aria-expanded="false"/g) ?? []).length, 2, panel);
  assert.match(panel, /id="need-plugin-up" data-card="plug:up"/);
  // The On/Off switch is in the card's actions, not inside the head's toggle button.
  const toggles = panel.match(/<button type="button" class="set-card-toggle"[\s\S]*?<\/button>/g) ?? [];
  assert.equal(toggles.length, 2);
  assert.ok(toggles.every((t) => !t.includes('class="switch')), toggles.join("\n"));
  assert.equal((panel.match(/class="row set-card-actions"><label class="switch/g) ?? []).length, 2, panel);
});

test("What's waiting: held plugin changes beside the import's to-dos and a restart, each with where it is", async () => {
  const { holdChange, snapshot } = await import("./controller.ts");
  const { waiting } = await import("./todo.ts");
  const { WaitingPill } = await import("./Setup.tsx");
  uploaderSetup();
  transport.onMessage({ type: "hello", version: "test", platform: "test", api: 1, access: "control", surveyBands: [], capabilities: [] });
  transport.onMessage({ type: "state", phase: "running", error: null, ended: false, restartNeeded: true });
  assert.equal(html(<WaitingPill />), '<div class="waiting-pill" role="status" aria-live="polite"><span><b>1</b> to do</span><button type="button" class="btn ghost small">Show</button></div>', "a restart alone: nothing to apply");
  holdChange({ form: { kind: "settings", plugin: "up" }, name: "Up", values: { server: "https://b", apiKey: "k" } });
  holdChange({ form: { kind: "copy", plugin: "up", system: 0, copy: 0 }, name: "Up", values: { label: "E" } });
  holdChange({ form: { kind: "site", plugin: "up", system: 1, copy: 0, site: 7 }, name: "Up", values: { k: 1 } });
  const all = waiting(snapshot());
  assert.deepEqual(all.map((w) => [w.title, w.target ?? null]), [
    ["Up: not applied", "plugin-up-held"],
    ["Up: not applied", "sysplug-east-up-0-copy-held"],
    ["Up: not applied", "sysplug-west-up-0-site7-held"],
    ["Restart to apply", null],
  ]);
  // The same form again replaces what it held.
  holdChange({ form: { kind: "copy", plugin: "up", system: 0, copy: 0 }, name: "Up", values: { label: "E2" } });
  assert.equal(snapshot().held.length, 3);
  assert.match(html(<WaitingPill />), /<b>4<\/b> to do.*>Apply all</);
  // Its Apply bar is where Show takes you.
  assert.match(html(<PluginSetupPanel />), /class="held-bar" role="status" id="need-plugin-up-held"/);
});

test("Apply all saves every held change in one config save, so each plugin restarts once", async () => {
  const { applyHeld, holdChange, snapshot } = await import("./controller.ts");
  const sent: { type: string; config?: Config }[] = [];
  transport.send = (m) => void sent.push(m as { type: string; config?: Config });
  uploaderSetup();
  transport.onMessage({ type: "state", phase: "running", error: null, ended: false, restartNeeded: false });
  applyHeld();
  holdChange({ form: { kind: "settings", plugin: "up" }, name: "Up", values: { server: "https://b", apiKey: "k" } });
  holdChange({ form: { kind: "copy", plugin: "up", system: 0, copy: 0 }, name: "Up", values: { label: "E" } });
  const before = sent.filter((m) => m.type === "setConfig").length;
  applyHeld();
  assert.equal(snapshot().held.length, 0);
  await new Promise((r) => setTimeout(r, 400));
  const saves = sent.filter((m) => m.type === "setConfig").slice(before);
  assert.equal(saves.length, 1, "one save");
  const saved = saves[0].config!;
  assert.equal(saved.plugins!.up.settings!.server, "https://b");
  assert.deepEqual(saved.systems[0].plugins?.up, { label: "E" });
});
