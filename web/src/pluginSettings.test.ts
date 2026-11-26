// A system's new name follows into the plugin settings that name it, and a system made sites of
// another takes its plugin settings for a site along. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { defaultConfig, newSystem } from "./config.ts";
import { joinPluginSettings, renameSystemRefs } from "./pluginSettings.ts";
import type { PluginInfo, Site } from "./protocol.ts";

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

test("a system made sites of another takes the settings a site may have of its own to them", () => {
  const c = defaultConfig();
  const site = (id: number, name: string): Site => ({ id, name, enabled: true, modulation: "qpsk", expect: [], controlChannelsHz: [851e6 + id], voiceChannelsHz: [] }) as unknown as Site;
  c.systems.push(
    { ...newSystem(c, { shortName: "dcfd" }), id: 1, sites: [site(10, "north")] },
    { ...newSystem(c, { shortName: "dcfd2" }), id: 2, sites: [site(20, "south"), site(21, "east")] },
  );
  const [to, from] = c.systems;
  to.plugins = { bcfy: { apiKey: "k", systemId: 77, talkgroupMode: "all" } };
  // Its own system ID, the same key, other talkgroups (which a site can't have), and one site's ID of its own.
  from.plugins = { bcfy: { apiKey: "k", systemId: 78, talkgroupMode: "only", perSite: { "21": { systemId: 79 } } }, other: { token: "t" } };
  c.plugins = { bcfy: { enabled: true }, other: { enabled: true, systemsOff: [2] } };
  const manifest = (name: string, site_overrides: string[]) => ({ name, site_overrides, system_config: { type: "object", properties: { apiKey: {}, systemId: {}, talkgroupMode: {} } } });
  const plugins = [
    { id: "bcfy", manifest: manifest("Broadcastify Calls", ["apiKey", "systemId"]) },
    { id: "other", manifest: manifest("Other", []) },
  ] as unknown as PluginInfo[];
  const dropped = joinPluginSettings(c, from, to, plugins);
  assert.deepEqual(to.plugins.bcfy, { apiKey: "k", systemId: 77, talkgroupMode: "all", perSite: { "20": { systemId: 78 }, "21": { systemId: 79 } } });
  assert.deepEqual(dropped, ["Broadcastify Calls", "Other"], "the talkgroups, and a plugin with nothing for a site");
  assert.deepEqual([c.plugins.other.systemsOff, c.plugins.other.sitesOff], [undefined, [20, 21]], "off for the system: off for its sites");
  // The same settings: nothing to carry, nothing dropped.
  const d = defaultConfig();
  d.systems.push({ ...newSystem(d, { shortName: "a" }), id: 1, sites: [site(10, "n")] }, { ...newSystem(d, { shortName: "b" }), id: 2, sites: [site(20, "s")] });
  d.systems[0].plugins = { bcfy: { apiKey: "k", systemId: 77 } };
  d.systems[1].plugins = { bcfy: { apiKey: "k", systemId: 77 } };
  assert.deepEqual(joinPluginSettings(d, d.systems[1], d.systems[0], plugins), []);
  assert.deepEqual(d.systems[0].plugins.bcfy, { apiKey: "k", systemId: 77 });
});
