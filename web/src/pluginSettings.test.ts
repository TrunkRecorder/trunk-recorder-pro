// A system's new name follows into the plugin settings that name it. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { defaultConfig, newSystem } from "./config.ts";
import { renameSystemRefs } from "./pluginSettings.ts";
import type { PluginInfo } from "./protocol.ts";

test("a renamed system follows into a plugin's system menus and lists", () => {
  const c = defaultConfig();
  c.systems.push(
    newSystem(c, { shortName: "alpha" }),
    newSystem(c, { shortName: "beta" }),
  );
  c.plugins = {
    mqtt: {
      enabled: true,
      settings: {
        home: "alpha",
        leaveOut: ["beta", "alpha"],
        other: ["alpha"],
      },
    },
  };
  const plugins = [
    {
      id: "mqtt",
      manifest: {
        config: {
          type: "object",
          properties: {
            home: { type: "string", "x-system": true },
            leaveOut: {
              type: "array",
              items: { type: "string" },
              "x-systems": true,
            },
            other: { type: "array", items: { type: "string" } },
          },
        },
      },
    },
  ] as unknown as PluginInfo[];
  renameSystemRefs(c, "alpha", "gamma", plugins);
  assert.deepEqual(
    c.plugins.mqtt.settings,
    { home: "gamma", leaveOut: ["beta", "gamma"], other: ["alpha"] },
    "a plain list isn't a list of systems",
  );
});
