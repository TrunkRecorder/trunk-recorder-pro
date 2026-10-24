// Sharing a system with the community directory: the switch says what is
// shared (and what isn't), how the last share went, links to the directory,
// and the config keeps the link. `npm test`.
import "./testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { defaultConfig, normalizeSystem, restartKey } from "./config.ts";
import { snapshot, transport } from "./controller.ts";
import type { Config, EngineStatus, ShareStatus, System } from "./protocol.ts";
import { directoryUrl, ShareSystem } from "./ShareSystem.tsx";

function connected(systems: System[], share: ShareStatus | null = null): Config {
  transport.send = () => {};
  transport.onConnection(true);
  const config = { ...defaultConfig(), systems };
  transport.onMessage({ type: "config", config, version: 1 });
  transport.onMessage({ type: "hello", version: "test", platform: "test", api: 1, surveyBands: [], capabilities: [] });
  const status = systems.map((s, index) => ({ index, shortName: s.shortName, activeCalls: 0, recording: 0, callsConcluded: 0, share, sites: [] }));
  transport.onMessage({ type: "status", status: { systems: status, recording: 0 } as unknown as EngineStatus, sources: [], load: 0, calls: [] });
  return snapshot().config!;
}

/** A P25 system with an FCC call sign (sharing needs one, for now). */
const p25 = (more: Partial<System> = {}) => normalizeSystem({ shortName: "dcfd", type: "p25", controlChannelsHz: [857_987_500], fcc: { "857987500": { callSign: "WPXT459", from: "air" } }, ...more });

test("Share: the switch is there without an FCC call sign (switching it on asks Identify); on in the file, it says it waits for one", () => {
  const c = connected([p25({ fcc: {} })]);
  const html = renderToString(<ShareSystem c={c} i={0} />);
  assert.ok(html.includes("Share channel and talkgroup config with the community"), html);
  assert.ok(!html.includes("checked"));
  const d = connected([p25({ fcc: {}, share: true })]);
  assert.ok(renderToString(<ShareSystem c={d} i={0} />).includes("Not shared until it has an FCC call sign"));
});

test("Share: the switch says what goes to the directory, and not names or a location", () => {
  const c = connected([p25()]);
  const html = renderToString(<ShareSystem c={c} i={0} />);
  assert.ok(html.includes("Share channel and talkgroup config with the community"), html);
  assert.ok(html.includes("Not talkgroup names, and not where you are"));
  assert.ok(!html.includes("checked"), "off until switched on");
});

test("Share: shared, with when and a link to its page in the directory", () => {
  const sys = p25({ share: true, directory: { id: "sys_abc" } });
  const c = connected([sys], { state: "shared", lastShared: Date.now() - 3600_000, error: null });
  const html = renderToString(<ShareSystem c={c} i={0} />);
  assert.ok(html.includes("Shared 1 h ago") || html.includes("Shared 60 m ago"), html);
  assert.ok(html.includes("https://trunkrecorder.pro/directory/systems/sys_abc"));
  assert.equal(directoryUrl({ ...c, server: { ...c.server, trpApi: "http://localhost:8790/" } }, "sys_abc"), "http://localhost:8790/directory/systems/sys_abc");
});

test("Share: its error, and conventional-only protocols can't be shared", () => {
  let c = connected([p25({ share: true })], { state: "error", lastShared: null, error: "Too many shares" });
  assert.ok(renderToString(<ShareSystem c={c} i={0} />).includes("Not shared: Too many shares"));
  c = connected([normalizeSystem({ shortName: "ed", type: "edacs", fcc: { "851012500": { callSign: "WQAA123" } } })]);
  const html = renderToString(<ShareSystem c={c} i={0} />);
  assert.ok(html.includes("can&#x27;t be shared yet") || html.includes("can't be shared yet"), html);
});

test("Share: the config keeps the switch and the directory's link", () => {
  const sys = normalizeSystem({ shortName: "dcfd", type: "p25", share: true, directory: { id: "sys_abc", rev: 4, sharedAt: 1 } as never, sites: [{ name: "north", directoryId: "site_n", controlChannelsHz: [1] }] });
  assert.equal(sys.share, true);
  assert.deepEqual(sys.directory, { id: "sys_abc" }, "only its ID: what sharing did is kept beside the config");
  assert.equal(sys.sites[0].directoryId, "site_n");
  assert.equal(normalizeSystem({ shortName: "x", type: "p25", share: false }).share, undefined);
});

test("Share: switching it, or a share saving the directory's link, needs no restart", () => {
  const before = { ...defaultConfig(), systems: [p25()] };
  const after = { ...before, systems: [{ ...p25({ share: true, directory: { id: "sys_abc" } }), sites: p25().sites.map((x) => ({ ...x, directoryId: "site_n" })) }] };
  assert.equal(restartKey(after, true), restartKey(before, true));
  assert.notEqual(restartKey({ ...before, systems: [p25({ name: "Other" })] }, true), restartKey(before, true));
});
