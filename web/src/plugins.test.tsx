// The Plugins page as rendered (react-dom/server): each plugin can be taken
// again from where it came, and uninstalled. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { defaultConfig } from "./config.ts";
import { transport } from "./controller.ts";
import { PluginsPage } from "./Plugins.tsx";
import type { PluginInfo } from "./protocol.ts";

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
