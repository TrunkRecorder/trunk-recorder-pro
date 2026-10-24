// A system from the systems directory (trunkrecorder.pro/directory), into
// this recorder: its bundle (format "trp-system": its config, its named
// talkgroups, where it came from) arrives by a link (#import=<the bundle's
// URL>, from the directory's "Open in Trunk Recorder Pro") or as a file
// (Setup's Import…). Either way it's shown first: which sites, what to call
// it, and, if the config already has the system (the same directory record,
// identity or control channels), updating that one instead of adding it again.

import { useEffect, useState, useSyncExternalStore } from "react";
import { newSiteName, normalizeSite, normalizeSystem, sameSystem, trpBase, type SystemInput } from "./config.ts";
import { setTalkgroups, snapshot, updateConfig, useSelect } from "./controller.ts";
import { cleanShortName } from "./fieldText.ts";
import { Modal } from "./Identify.tsx";
import type { Config, Site, System } from "./protocol.ts";
import { mergeTalkgroupCsv, parseTalkgroupCsv } from "./talkgroups.ts";

export interface Bundle {
  format: "trp-system";
  v: 1;
  source: { directory: string; id: string; rev: number; url?: string; licensee?: string };
  system: SystemInput & { sites: Partial<Site>[] };
  talkgroupsCsv: string;
}

/** The page was opened to import a system (#import=…): the first-run guide waits. (Read as it loads: the import takes the link off.) */
export const ARRIVED_TO_IMPORT = typeof location !== "undefined" && location.hash.startsWith("#import=");

/** A directory bundle, or null when `x` isn't one. */
export function parseBundle(x: unknown): Bundle | null {
  const b = x as Partial<Bundle> | null;
  if (!b || typeof b !== "object" || b.format !== "trp-system" || b.v !== 1) return null;
  if (!b.system || typeof b.system !== "object" || !Array.isArray(b.system.sites) || !b.source?.id) return null;
  return { ...(b as Bundle), talkgroupsCsv: typeof b.talkgroupsCsv === "string" ? b.talkgroupsCsv : "" };
}

/**
 * The bundle's URL a link asks to import (#import=…): the URL itself, or (an
 * older link) a directory system ID and its sites, at `directory`.
 */
export function importUrl(hash: string, directory: string): string | null {
  const m = /^#import=([^&]+)(?:&sites=([^&]*))?/.exec(hash);
  if (!m) return null;
  const v = decodeURIComponent(m[1]);
  if (/^https?:\/\//.test(v)) return v;
  if (/^sys_[0-9a-z]+$/.test(v)) return `${directory.replace(/\/+$/, "")}/directory/api/v1/systems/${v}/config${m[2] ? `?sites=${m[2]}` : ""}`;
  return null;
}

/**
 * The system in `c` that is the bundle's (to update, not add again): by its
 * directory record, else by identity (sameSystem: the same WACN and System
 * ID) or a control channel in common, of the same type. -1: none.
 */
export function findExisting(c: Config, b: Bundle): number {
  const byLink = c.systems.findIndex((s) => s.directory?.id === b.source.id);
  if (byLink >= 0) return byLink;
  const ids = b.system.sites.map((s) => s.expect ?? {});
  const ccs = new Set(b.system.sites.flatMap((s) => s.controlChannelsHz ?? []));
  return c.systems.findIndex((s) => s.type === b.system.type && s.sites.some((x) => ids.some((id) => sameSystem(x.expect, id)) || x.controlChannelsHz.some((f) => ccs.has(f))));
}

/** A short name for the bundle's system none in `c` has: its own, else from its name. */
export function freeShortName(c: Config, wanted: string): string {
  const base = cleanShortName(wanted.toLowerCase().replace(/[^a-z0-9_-]+/g, "-").replace(/^-+|-+$/g, "")) || "system";
  const taken = new Set([...c.systems, ...c.conventional].map((s) => s.shortName));
  if (!taken.has(base)) return base;
  for (let n = 2; ; n++) if (!taken.has(`${base}-${n}`)) return `${base}-${n}`;
}

/** Add the bundle's system (these sites of it) as `shortName`; its talkgroups become its talkgroup file. */
export function addSystem(b: Bundle, shortName: string, sites: number[], talkgroups: Record<string, string>): void {
  updateConfig((x) => {
    const sys = normalizeSystem({ ...b.system, shortName, enabled: true, sites: b.system.sites.filter((_, k) => sites.includes(k)) });
    x.systems.push(sys);
  });
  if (b.talkgroupsCsv.trim()) setTalkgroups(shortName, mergeTalkgroupCsv(talkgroups[shortName] ?? "", b.talkgroupsCsv).csv);
}

/**
 * Update system `i` of the config with the bundle's: its directory link,
 * these sites (a site it has, by directory ID or control channel, gets the
 * link and any channels it lacks; others are added), licenses and talkgroups
 * it lacks. Nothing of the user's is replaced. Returns the talkgroups added.
 */
export function updateSystem(b: Bundle, i: number, sites: number[], talkgroups: Record<string, string>): number {
  let shortName = "";
  updateConfig((x) => {
    const s = x.systems[i];
    shortName = s.shortName;
    s.directory = b.system.directory ?? { id: b.source.id, rev: b.source.rev };
    if (!s.name && b.system.name) s.name = b.system.name;
    for (const k of sites) {
      const n = b.system.sites[k];
      const have = s.sites.find((y) => (n.directoryId && y.directoryId === n.directoryId) || y.controlChannelsHz.some((f) => (n.controlChannelsHz ?? []).includes(f)));
      if (have) {
        if (n.directoryId) have.directoryId = n.directoryId;
        have.controlChannelsHz = [...new Set([...have.controlChannelsHz, ...(n.controlChannelsHz ?? [])])];
        have.voiceChannelsHz = [...new Set([...have.voiceChannelsHz, ...(n.voiceChannelsHz ?? [])])];
        have.expect = { ...(n.expect ?? {}), ...have.expect };
      } else {
        const site = normalizeSite({ ...n }, s);
        site.name = n.name?.trim() || newSiteName(s, n.expect);
        s.sites.push(site);
      }
    }
    for (const [k, v] of Object.entries(b.system.fcc ?? {})) {
      s.fcc ??= {};
      s.fcc[k] ??= v;
    }
  });
  if (!b.talkgroupsCsv.trim()) return 0;
  const m = mergeTalkgroupCsv(talkgroups[shortName] ?? "", b.talkgroupsCsv);
  if (m.added) setTalkgroups(shortName, m.csv);
  return m.added;
}

// ── what's being imported (one at a time), for the dialog ──────────────────────

type Pending = { state: "loading"; url: string } | { state: "ready"; bundle: Bundle } | { state: "failed"; why: string } | null;
let pending: Pending = null;
const listeners = new Set<() => void>();
const setPending = (p: Pending) => ((pending = p), listeners.forEach((f) => f()));

/** Show the bundle's import. */
export function openImport(bundle: Bundle): void {
  setPending({ state: "ready", bundle });
}

/** Fetch a bundle from `url`, then show its import. */
export async function openImportUrl(url: string): Promise<void> {
  setPending({ state: "loading", url });
  try {
    const r = await fetch(url);
    if (!r.ok) throw new Error(r.status === 404 ? "The directory has no such system." : `The directory answered HTTP ${r.status}.`);
    const b = parseBundle(await r.json());
    if (!b) throw new Error("That isn't a system from the directory.");
    setPending({ state: "ready", bundle: b });
  } catch (e) {
    setPending({ state: "failed", why: `Couldn't get it from the directory: ${e instanceof Error ? e.message : String(e)}` });
  }
}

/** The import dialog, when there is one; and a link's import (#import=…), once the config has come. Mounted once, at the app's root. */
export function DirectoryImport() {
  const p = useSyncExternalStore(
    (f) => (listeners.add(f), () => void listeners.delete(f)),
    () => pending,
    () => null,
  );
  const ready = useSelect((s) => !!s.config);
  useEffect(() => {
    if (!ready || typeof location === "undefined") return;
    const url = importUrl(location.hash, trpBase(snapshot().config));
    if (!url) return;
    history.replaceState(null, "", location.pathname + location.search);
    void openImportUrl(url);
  }, [ready]);
  if (!p) return null;
  const close = () => setPending(null);
  if (p.state === "loading")
    return (
      <Modal title="Import from the directory" onClose={close}>
        <p className="muted">Getting it from the directory…</p>
      </Modal>
    );
  if (p.state === "failed")
    return (
      <Modal title="Import from the directory" onClose={close}>
        <div className="banner bad small">{p.why}</div>
        <div className="row">
          <span className="spacer" />
          <button className="btn" onClick={close}>
            Close
          </button>
        </div>
      </Modal>
    );
  return <ImportDialog bundle={p.bundle} onClose={close} />;
}

/** FCC licensees are in capitals: "DISTRICT OF COLUMBIA" → "District of Columbia". */
const titleCase = (s: string) => (s !== s.toUpperCase() ? s : s.toLowerCase().replace(/\b([a-z])([a-z']*)/g, (w, a: string, r: string, i: number) => (i > 0 && ["of", "and", "the", "for", "in", "on", "at", "to"].includes(w) ? w : a.toUpperCase() + r)));
const mhz = (hz: number) => (hz / 1e6).toFixed(5).replace(/0{1,2}$/, "");
const TYPES: Record<string, string> = { p25: "P25", smartnet: "SmartNet", dmr: "DMR", nxdn: "NXDN", edacs: "EDACS" };

export function ImportDialog(props: { bundle: Bundle; onClose: () => void }) {
  const b = props.bundle;
  const s = useSelect((x) => ({ config: x.config, talkgroups: x.talkgroups }));
  const c = s.config;
  const [sites, setSites] = useState<number[]>(() => b.system.sites.map((_, k) => k));
  const same = c ? findExisting(c, b) : -1;
  const [mode, setMode] = useState<"update" | "add">(same >= 0 ? "update" : "add");
  const [name, setName] = useState(() => (c ? freeShortName(c, b.system.shortName ?? b.system.name ?? "system") : "system"));
  const [done, setDone] = useState<string | null>(null);
  if (!c) return null;
  const tgCount = parseTalkgroupCsv(b.talkgroupsCsv).size;
  const licenses = Object.keys(b.system.fcc ?? {}).length;
  const taken = [...c.systems, ...c.conventional].some((x) => x.shortName === name);
  const existing: System | undefined = same >= 0 ? c.systems[same] : undefined;
  const go = () => {
    if (mode === "update" && existing) {
      const added = updateSystem(b, same, sites, s.talkgroups);
      setDone(`Updated ${existing.shortName}: its directory link, the sites and licenses it lacked, and ${added} talkgroup${added === 1 ? "" : "s"} it didn't have.`);
    } else {
      addSystem(b, name, sites, s.talkgroups);
      setDone(`Added ${name}${tgCount ? `, with ${tgCount} talkgroup${tgCount === 1 ? "" : "s"}` : ""}. Setup → Systems has it; the to-do list says if no radio covers its frequencies yet.`);
    }
  };
  return (
    <Modal title={`Import ${b.system.name || (b.source.licensee ? titleCase(b.source.licensee) : "") || b.system.shortName || "a system"}`} onClose={props.onClose}>
      {done ? (
        <>
          <p>{done}</p>
          <div className="row">
            <span className="spacer" />
            <button className="btn primary" onClick={props.onClose}>
              Done
            </button>
          </div>
        </>
      ) : (
        <>
          <p className="small muted">
            {`${TYPES[b.system.type ?? ""] ?? b.system.type} · ${b.system.sites.length} site${b.system.sites.length === 1 ? "" : "s"} · ${tgCount} named talkgroup${tgCount === 1 ? "" : "s"}${licenses ? ` · ${licenses} FCC license${licenses === 1 ? "" : "s"}` : ""} · from `}
            <a href={b.source.url ?? `${b.source.directory}/systems/${b.source.id}`} target="_blank" rel="noreferrer">
              the directory
            </a>
            {` (version ${b.source.rev})`}
          </p>
          <fieldset className="stack">
            <legend className="small">Sites</legend>
            {b.system.sites.map((x, k) => (
              <label key={k} className="toggle small">
                <input type="checkbox" checked={sites.includes(k)} onChange={(e) => setSites(e.target.checked ? [...sites, k] : sites.filter((y) => y !== k))} />
                <span>
                  {x.name || (x.expect?.rfss != null ? `Site ${x.expect.rfss}-${x.expect.site}` : `Site ${k + 1}`)} · <span className="mono">{(x.controlChannelsHz ?? []).map(mhz).join(", ")}</span> MHz
                </span>
              </label>
            ))}
          </fieldset>
          {existing && (
            <fieldset className="stack">
              <legend className="small">You have it already: {existing.shortName}</legend>
              <label className="toggle small">
                <input type="radio" name="import-mode" checked={mode === "update"} onChange={() => setMode("update")} />
                Update {existing.shortName}: add what it lacks (sites, licenses, talkgroups). Nothing of yours is replaced.
              </label>
              <label className="toggle small">
                <input type="radio" name="import-mode" checked={mode === "add"} onChange={() => setMode("add")} />
                Add it as another system
              </label>
            </fieldset>
          )}
          {mode === "add" && (
            <label className="field">
              <span className="field-label">Short name</span>
              <input className="search" value={name} onChange={(e) => setName(cleanShortName(e.target.value))} />
              <span className="field-hint">{taken ? "Already used: each system needs its own." : "Its folder for calls, and how plugins know it."}</span>
            </label>
          )}
          <div className="row">
            <span className="spacer" />
            <button className="btn ghost" onClick={props.onClose}>
              Cancel
            </button>
            <button className="btn primary" disabled={!sites.length || (mode === "add" && (!name || taken))} onClick={go}>
              {mode === "update" && existing ? `Update ${existing.shortName}` : "Add it"}
            </button>
          </div>
        </>
      )}
    </Modal>
  );
}
