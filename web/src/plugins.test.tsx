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
  id, path: `/plugins/${id}/${id}`, custom: false, problem: null, perSystem: true,
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

test("Setup → Plugins: a plugin is turned on and off system by system, when everything it takes is one system's", () => {
  const c = uploaderSetup();
  c.plugins!.up.systemsOff = [2];
  transport.onMessage({ type: "config", config: c, version: 2 });
  const panel = html(<PluginSetupPanel />);
  assert.match(panel, /aria-label="Systems Up is on for"/);
  assert.match(panel, /aria-pressed="true"[^>]*>east</);
  assert.match(panel, /aria-pressed="false"[^>]*>west</);
  // One that takes every system's figures can't be.
  transport.onMessage({ type: "plugins", encoderFound: null, plugins: [plugin("up", { perSystem: false })] });
  assert.doesNotMatch(renderToString(<PluginSetupPanel />), /Systems Up is on for/);
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
