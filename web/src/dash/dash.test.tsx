// The dashboard pages as rendered (react-dom/server), from what the recorder
// sends: what a user relies on seeing there stays there. `npm test`.
import "../testEnv.ts";
import assert from "node:assert/strict";
import { test } from "node:test";
import { renderToString } from "react-dom/server";
import { defaultConfig, newSystem } from "../config.ts";
import { setView, transport } from "../controller.ts";
import type { Config, EngineStatus, FromRecorder, Identity, System, SystemStatus } from "../protocol.ts";
import { DecodePage } from "./Decode.tsx";
import { Overview } from "./Overview.tsx";

function config(...systems: Partial<System>[]): Config {
  const c = defaultConfig();
  systems.forEach((x, i) => c.systems.push(newSystem(c, { controlChannelsHz: [851_000_000 + i * 25_000], ...x })));
  return c;
}

function status(shortName: string, index: number, siteGroup: string | null, identity: Partial<Identity>): SystemStatus {
  return {
    index, shortName, nowS: 10, controlChannelHz: 851_000_000 + index * 25_000,
    identity: { nac: 0x1f1, wacn: null, sysId: null, rfss: 1, site: null, ...identity },
    good: 100, bad: 0, modulation: null, activeCalls: 0, recording: 0, callsConcluded: 0, mismatch: null, adjacent: [], siteGroup,
  };
}

/** The recorder says hello with `c` and is running these systems. */
function running(c: Config, systems: SystemStatus[]): void {
  const hello = { type: "hello", version: "test", platform: "test", config: c, devices: [], history: [], phase: { type: "state", phase: "running", error: null, ended: false } };
  transport.onMessage(hello as unknown as FromRecorder);
  transport.onMessage({ type: "status", status: { systems, recording: 0 } as unknown as EngineStatus, sources: [], load: 0, calls: [] });
}

/** The DCFD-like case: two sites of one P25 system set up apart, another system between them. */
function twoSites(): void {
  const c = config({ shortName: "site1" }, { shortName: "other" }, { shortName: "site2" });
  running(c, [status("site1", 0, "p25:bee00.1a2", { wacn: 0xbee00, sysId: 0x1a2, site: 1 }), status("other", 1, "p25:bee00.999", { wacn: 0xbee00, sysId: 0x999, site: 1 }), status("site2", 2, "p25:bee00.1a2", { wacn: 0xbee00, sysId: 0x1a2, site: 2 })]);
}

const text = (html: string) => html.replace(/<!-- -->/g, "").replace(/<[^>]+>/g, " ").replace(/\s+/g, " ");
const titles = (html: string) => [...html.matchAll(/<h3>(.*?)<\/h3>/g)].map((m) => text(m[1]).trim());

test("Overview: each site of a multi-site system says which others it is recorded with", () => {
  twoSites();
  setView("overview");
  const page = text(renderToString(<Overview />));
  assert.match(page, /multi-site with site2/);
  assert.match(page, /multi-site with site1/);
  assert.doesNotMatch(page, /multi-site with other/);
  assert.equal(page.match(/multi-site with/g)?.length, 2, "the system on its own has no chip");
});

test("Overview: the sites of one system are next to each other", () => {
  twoSites();
  setView("overview");
  const shown = titles(renderToString(<Overview />)).filter((x) => ["site1", "site2", "other"].includes(x));
  assert.deepEqual(shown, ["site1", "site2", "other"]);
});

test("Decode, one system: says which sites it is recorded with", () => {
  twoSites();
  setView("decode", "site2");
  assert.match(text(renderToString(<DecodePage />)), /multi-site with site1/);
  setView("decode", "other");
  assert.doesNotMatch(text(renderToString(<DecodePage />)), /multi-site with/);
});

test("Overview: a site group joins DMR sites; one switched off isn't listed", () => {
  const c = config({ shortName: "cm1", type: "dmr", siteGroup: "capmax" }, { shortName: "cm2", type: "dmr", siteGroup: "capmax" }, { shortName: "cm3", type: "dmr", siteGroup: "capmax", enabled: false });
  running(c, [status("cm1", 0, "group:capmax", {}), status("cm2", 1, "group:capmax", {})]);
  setView("overview");
  const page = renderToString(<Overview />);
  assert.match(text(page), /multi-site with cm2/);
  assert.doesNotMatch(text(page), /cm3/);
  assert.match(page, /title="Site group &quot;capmax&quot; — calls saved once"/);
});
