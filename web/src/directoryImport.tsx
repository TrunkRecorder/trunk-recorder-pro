// A system from the systems directory (trunkrecorder.pro/directory), into
// this recorder: its bundle (format "trp-system": its config, its named
// talkgroups, where it came from) arrives by a link (#import=<the bundle's
// URL>, from the directory's "Open in Trunk Recorder Pro") or as a file
// (Setup's Import…). Either way it's shown first: which sites, what to call
// it, and, if the config already has the system (the same directory record,
// identity, control channels or licenses), updating that one instead of adding
// it again. A system no one has shared comes known only by its licenses (no
// site to follow yet): the survey looks for it, marking the signals it finds
// on its licensed frequencies, and adding what it finds gives it them. A
// system with sites can be checked first: which of them can be heard here
// (the survey listens to their frequencies), the ones that can kept ticked.
// A RadioReference sites export (trs_sites_NNNN.csv) imports the same way,
// its kind of system chosen here, the sites far away left out.

import { useEffect, useState, useSyncExternalStore } from "react";
import { fccPrimary, newSiteName, normalizeSite, normalizeSystem, sameSystem, trpBase, type SystemInput } from "./config.ts";
import { findOnAir, findPlaces, setTalkgroups, snapshot, updateConfig, useSelect } from "./controller.ts";
import { bundleSiteName, lookForBundle } from "./lookFor.ts";
import { cleanShortName } from "./fieldText.ts";
import { Modal } from "./Identify.tsx";
import type { Config, Site, System, Talkgroup } from "./protocol.ts";
import { mergeTalkgroups, readTalkgroupFile, splitCsvLine } from "./talkgroups.ts";

export interface Bundle {
  format: "trp-system";
  v: 1;
  source: { directory: string; id: string; rev: number; url?: string; licensee?: string };
  system: SystemInput & { sites: Partial<Site>[] };
  talkgroupsCsv: string;
  /** Where each site is (lat, lon), where known (a sites file says). */
  locations?: ([number, number] | null)[];
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
 * A RadioReference sites export (trs_sites_NNNN.csv: a site a row, its
 * frequencies, MHz, from the Frequencies column to the row's end, a `c`
 * marking a control channel) as a bundle: its sites, their control
 * channels (none marked: all of them; the rest as voice channels), RFSS,
 * site and NAC, and where they are. Its kind of system isn't in it: the
 * import asks. Null when `text` isn't one.
 */
export function sitesFileBundle(text: string, fileName: string): Bundle | null {
  const lines = text.replace(/^\uFEFF/, "").split(/\r?\n/).filter((l) => l.trim());
  if (lines.length < 2) return null;
  const head = splitCsvLine(lines[0]).map((h) => h.trim().toLowerCase());
  const col = (...names: string[]) => head.findIndex((h) => names.includes(h));
  const fcol = col("frequencies", "frequency", "freqs");
  if (fcol < 0) return null;
  const [cSite, cRfss, cNac, cName, cLat, cLon] = [col("site dec", "site", "site number"), col("rfss"), col("site nac", "nac"), col("description", "name", "site name"), col("lat", "latitude"), col("lon", "long", "longitude")];
  const sites: Partial<Site>[] = [];
  const locations: ([number, number] | null)[] = [];
  for (const line of lines.slice(1)) {
    const f = splitCsvLine(line).map((x) => x.trim());
    const get = (c: number) => (c >= 0 && f[c] ? f[c] : undefined);
    const freqs = f.slice(fcol).flatMap((v) => {
      const mhz = parseFloat(v.replace(/[ca]$/i, ""));
      return mhz > 1 && /^\d+(\.\d+)?[ca]?$/i.test(v) ? [{ hz: Math.round((mhz * 1e6) / 50) * 50, control: /c$/i.test(v) }] : [];
    });
    if (!freqs.length) continue;
    const control = freqs.filter((x) => x.control).map((x) => x.hz);
    const num = (c: number, radix = 10) => (get(c) && !isNaN(parseInt(get(c)!, radix)) ? parseInt(get(c)!, radix) : undefined);
    const expect = Object.fromEntries(Object.entries({ rfss: num(cRfss), site: num(cSite), nac: num(cNac, 16) }).filter(([, v]) => v !== undefined));
    sites.push({
      name: [get(cSite), get(cName)].filter(Boolean).join(" ") || undefined,
      controlChannelsHz: control.length ? control : freqs.map((x) => x.hz),
      voiceChannelsHz: control.length ? freqs.filter((x) => !x.control).map((x) => x.hz) : [],
      expect,
    });
    const [lat, lon] = [parseFloat(get(cLat) ?? ""), parseFloat(get(cLon) ?? "")];
    locations.push(isFinite(lat) && isFinite(lon) && (lat !== 0 || lon !== 0) ? [lat, lon] : null);
  }
  if (!sites.length) return null;
  const id = fileName.replace(/\.csv$/i, "");
  return { format: "trp-system", v: 1, source: { directory: "", id, rev: 0 }, system: { shortName: id.replace(/^trs_sites_/, "rr-"), sites }, talkgroupsCsv: "", locations };
}

/** A bundle's system as `type`: a sites file's sites keep their site lock only as P25 (its other kinds' numbers aren't what their air says). */
export function asType(b: Bundle, type: NonNullable<SystemInput["type"]>): Bundle {
  if (b.system.type) return b;
  return { ...b, system: { ...b.system, type, sites: b.system.sites.map((x) => (type === "p25" ? x : { ...x, expect: {} })) } };
}

/** How far apart two places are, miles (haversine). */
export function miles(a: [number, number], b: [number, number]): number {
  const r = Math.PI / 180;
  const h = Math.sin(((b[0] - a[0]) * r) / 2) ** 2 + Math.cos(a[0] * r) * Math.cos(b[0] * r) * Math.sin(((b[1] - a[1]) * r) / 2) ** 2;
  return 2 * 3958.8 * Math.asin(Math.sqrt(h));
}

/** A system known only by its licenses (no one has shared it): no site to follow yet, its licensed frequencies to find it by. */
export const toFind = (b: Bundle) => !b.system.sites.length && Object.keys(b.system.fcc ?? {}).length > 0;

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
 * ID) or a control channel in common, of the same type; else, known only by
 * its licenses, one with its call sign. -1: none.
 */
export function findExisting(c: Config, b: Bundle): number {
  const byLink = c.systems.findIndex((s) => s.directory?.id === b.source.id);
  if (byLink >= 0) return byLink;
  const ids = b.system.sites.map((s) => s.expect ?? {});
  const ccs = new Set(b.system.sites.flatMap((s) => s.controlChannelsHz ?? []));
  const same = c.systems.findIndex((s) => s.type === b.system.type && s.sites.some((x) => ids.some((id) => sameSystem(x.expect, id)) || x.controlChannelsHz.some((f) => ccs.has(f))));
  if (same >= 0 || !toFind(b)) return same;
  const calls = new Set(Object.values(b.system.fcc ?? {}).map((l) => l.callSign));
  return c.systems.findIndex((s) => Object.values(s.fcc ?? {}).some((l) => calls.has(l.callSign)));
}

/** A short name for the bundle's system none in `c` has: its own, else from its name. */
export function freeShortName(c: Config, wanted: string): string {
  const base = cleanShortName(wanted.toLowerCase().replace(/[^a-z0-9_-]+/g, "-").replace(/^-+|-+$/g, "")) || "system";
  const taken = new Set([...c.systems, ...c.conventional].map((s) => s.shortName));
  if (!taken.has(base)) return base;
  for (let n = 2; ; n++) if (!taken.has(`${base}-${n}`)) return `${base}-${n}`;
}

/** The bundle's talkgroups (its talkgroup file, read). */
const bundleTalkgroups = (b: Bundle): Talkgroup[] => (b.talkgroupsCsv.trim() ? readTalkgroupFile(b.talkgroupsCsv).talkgroups : []);

/** Add the bundle's system (these sites of it) as `shortName`, with its talkgroups. */
export function addSystem(b: Bundle, shortName: string, sites: number[], talkgroups: Record<string, Talkgroup[]>): void {
  updateConfig((x) => {
    const sys = normalizeSystem({ ...b.system, shortName, enabled: true, sites: b.system.sites.filter((_, k) => sites.includes(k)) });
    x.systems.push(sys);
  });
  const theirs = bundleTalkgroups(b);
  if (theirs.length) setTalkgroups(shortName, mergeTalkgroups(talkgroups[shortName] ?? [], theirs).talkgroups);
}

/**
 * Update system `i` of the config with the bundle's: its directory link,
 * these sites (a site it has, by directory ID or control channel, gets the
 * link and any channels it lacks; others are added), licenses and talkgroups
 * it lacks. Nothing of the user's is replaced. Returns the talkgroups added.
 */
export function updateSystem(b: Bundle, i: number, sites: number[], talkgroups: Record<string, Talkgroup[]>): number {
  let shortName = "";
  updateConfig((x) => {
    const s = x.systems[i];
    shortName = s.shortName;
    if (b.source.directory) s.directory = { id: b.system.directory?.id ?? b.source.id };
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
  const m = mergeTalkgroups(talkgroups[shortName] ?? [], bundleTalkgroups(b));
  if (m.added) setTalkgroups(shortName, m.talkgroups);
  return m.added;
}

// ── what's being imported (one at a time), for the dialog ──────────────────────

type Pending = { state: "loading"; url: string } | { state: "ready"; bundle: Bundle; sites?: number[] } | { state: "failed"; why: string } | null;
let pending: Pending = null;
const listeners = new Set<() => void>();
const setPending = (p: Pending) => ((pending = p), listeners.forEach((f) => f()));

/** Show the bundle's import (`sites`: these of its sites ticked; absent: all). */
export function openImport(bundle: Bundle, sites?: number[]): void {
  setPending({ state: "ready", bundle, sites });
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
  return <ImportDialog bundle={p.bundle} sites={p.sites} onClose={close} />;
}

/** FCC licensees are in capitals: "DISTRICT OF COLUMBIA" → "District of Columbia". */
const titleCase = (s: string) => (s !== s.toUpperCase() ? s : s.toLowerCase().replace(/\b([a-z])([a-z']*)/g, (w, a: string, r: string, i: number) => (i > 0 && ["of", "and", "the", "for", "in", "on", "at", "to"].includes(w) ? w : a.toUpperCase() + r)));
const mhz = (hz: number) => (hz / 1e6).toFixed(5).replace(/0{1,2}$/, "");
const TYPES: Record<string, string> = { p25: "P25", smartnet: "SmartNet", dmr: "DMR", nxdn: "NXDN", edacs: "EDACS" };
/** What a bundle's system is called: its name, else who holds its licenses. */
export const nameOf = (b: Bundle) => b.system.name || (b.source.licensee ? titleCase(b.source.licensee) : "") || b.system.shortName || "a system";

export function ImportDialog(props: { bundle: Bundle; sites?: number[]; onClose: () => void }) {
  const s = useSelect((x) => ({ config: x.config, talkgroups: x.talkgroups }));
  const c = s.config;
  // A sites file doesn't say what kind of system it is.
  const [kind, setKind] = useState<NonNullable<SystemInput["type"]>>(props.bundle.system.type ?? "p25");
  const b = asType(props.bundle, kind);
  const [sites, setSites] = useState<number[]>(() => props.sites ?? b.system.sites.map((_, k) => k));
  const same = c ? findExisting(c, b) : -1;
  const [mode, setMode] = useState<"update" | "add">(same >= 0 ? "update" : "add");
  const [name, setName] = useState(() => (c ? freeShortName(c, b.system.shortName ?? b.system.name ?? "system") : "system"));
  const [done, setDone] = useState<string | null>(null);
  const find = toFind(b);
  if (!c) return null;
  const tgCount = bundleTalkgroups(b).length;
  const licenses = Object.keys(b.system.fcc ?? {}).length;
  const taken = [...c.systems, ...c.conventional].some((x) => x.shortName === name);
  const existing: System | undefined = same >= 0 ? c.systems[same] : undefined;
  const fromFile = !props.bundle.source.directory;
  const look = (only?: number[]) => {
    const why = findOnAir(lookForBundle(b, nameOf(b), only));
    return why ? setDone(`${why} The survey is in Setup → Systems, looking for it when you scan.`) : props.onClose();
  };
  const go = () => {
    if (find && mode === "add") return look();
    if (mode === "update" && existing) {
      const added = updateSystem(b, same, sites, s.talkgroups);
      setDone(`Updated ${existing.shortName}: its ${fromFile ? "" : "directory link, the "}sites and licenses it lacked, and ${added} talkgroup${added === 1 ? "" : "s"} it didn't have.`);
    } else {
      addSystem(b, name, sites, s.talkgroups);
      setDone(`Added ${name}${tgCount ? `, with ${tgCount} talkgroup${tgCount === 1 ? "" : "s"}` : ""}. Setup → Systems has it; the to-do list says if no radio covers its frequencies yet.`);
    }
  };
  const source = fromFile ? (
    <>{`${props.bundle.source.id}.csv`}</>
  ) : (
    <a href={b.source.url ?? `${b.source.directory}/systems/${b.source.id}`} target="_blank" rel="noreferrer">
      the directory
    </a>
  );
  return (
    <Modal title={`${find ? "Find" : "Import"} ${fromFile ? `${props.bundle.source.id}.csv` : nameOf(b)}`} onClose={props.onClose}>
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
          {find ? (
            <>
              <p className="small muted">
                {`${fccPrimary(b.system.fcc, [])?.callSign ?? ""} · ${licenses} licensed frequenc${licenses === 1 ? "y" : "ies"} · from `}
                {source}
              </p>
              <p>
                No one has shared it yet, so all that's known is its licenses. They don't say which frequency is its control channel, so the survey looks for it: it listens to these frequencies, the signals on them are marked as its own, and the
                control channel it listens to sets it up.
              </p>
            </>
          ) : (
          <p className="small muted">
            {`${fromFile ? "RadioReference sites" : (TYPES[b.system.type ?? ""] ?? b.system.type)} · ${b.system.sites.length} site${b.system.sites.length === 1 ? "" : "s"}${fromFile ? "" : ` · ${tgCount} named talkgroup${tgCount === 1 ? "" : "s"}`}${licenses ? ` · ${licenses} FCC license${licenses === 1 ? "" : "s"}` : ""} · from `}
            {source}
            {!fromFile && ` (version ${b.source.rev})`}
          </p>
          )}
          {!props.bundle.system.type && (
            <label className="field">
              <span className="field-label">Kind of system</span>
              <select value={kind} onChange={(e) => setKind(e.target.value as typeof kind)}>
                {Object.entries(TYPES).map(([k, label]) => (
                  <option key={k} value={k}>
                    {label}
                  </option>
                ))}
              </select>
              <span className="field-hint">A sites file doesn't say: as RadioReference lists the system.</span>
            </label>
          )}
          {!find && b.locations?.some(Boolean) && <Near b={b} onPick={setSites} />}
          {!find && (
          <fieldset className="stack">
            <legend className="small">Sites</legend>
            {b.system.sites.map((x, k) => (
              <label key={k} className="toggle small">
                <input type="checkbox" checked={sites.includes(k)} onChange={(e) => setSites(e.target.checked ? [...sites, k] : sites.filter((y) => y !== k))} />
                <span>
                  {bundleSiteName(x, k)} · <span className="mono">{(x.controlChannelsHz ?? []).map(mhz).join(", ")}</span> MHz
                </span>
              </label>
            ))}
          </fieldset>
          )}
          {existing && (
            <fieldset className="stack">
              <legend className="small">You have it already: {existing.shortName}</legend>
              <label className="toggle small">
                <input type="radio" name="import-mode" checked={mode === "update"} onChange={() => setMode("update")} />
                Update {existing.shortName}: add what it lacks ({find ? "licenses" : "sites, licenses, talkgroups"}). Nothing of yours is replaced.
              </label>
              <label className="toggle small">
                <input type="radio" name="import-mode" checked={mode === "add"} onChange={() => setMode("add")} />
                {find ? "Find it on the air as another system" : "Add it as another system"}
              </label>
            </fieldset>
          )}
          {mode === "add" && !find && (
            <label className="field">
              <span className="field-label">Short name</span>
              <input className="search" value={name} onChange={(e) => setName(cleanShortName(e.target.value))} />
              <span className="field-hint">{taken ? "Already used: each needs its own." : "Its folder for calls, and how plugins know it."}</span>
            </label>
          )}
          {!find && (
            <p className="small muted">Not sure which sites reach you? Check them on the air first: the survey listens to each one's control channels and keeps the ones it hears ticked.</p>
          )}
          <div className="row">
            <span className="spacer" />
            <button className="btn ghost" onClick={props.onClose}>
              Cancel
            </button>
            {!find && (
              <button className="btn" disabled={!sites.length} onClick={() => look(sites)} title="Listen to each ticked site's control channels">
                Check which I can hear
              </button>
            )}
            <button className="btn primary" disabled={!find && (!sites.length || (mode === "add" && (!name || taken)))} onClick={go}>
              {mode === "update" && existing ? `Update ${existing.shortName}` : find ? "Start the survey" : "Add it"}
            </button>
          </div>
        </>
      )}
    </Modal>
  );
}

/** Only the sites within some miles of a place (a city, a ZIP code: the directory's place search). */
function Near(props: { b: Bundle; onPick: (sites: number[]) => void }) {
  const [q, setQ] = useState("");
  const [within, setWithin] = useState(40);
  const [note, setNote] = useState<string | null>(null);
  const pick = async () => {
    const at = (await findPlaces(q.trim()))[0];
    if (!at) return setNote("No such place found (or the directory couldn't be reached).");
    const locs = props.b.locations ?? [];
    const near = props.b.system.sites.map((_, k) => k).filter((k) => locs[k] && miles([at.lat, at.lon], locs[k]!) <= within);
    props.onPick(near);
    setNote(`${near.length} of ${props.b.system.sites.length} sites within ${within} miles of ${at.name}, ${at.state}.`);
  };
  return (
    <div className="stack">
      <div className="row">
        <input className="search" placeholder="Near: a city or ZIP code" value={q} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => e.key === "Enter" && void pick()} aria-label="Near" />
        <label className="row small">
          within
          <input type="number" min={1} max={500} value={within} onChange={(e) => setWithin(Math.max(1, Number(e.target.value) || 40))} style={{ width: "5em" }} aria-label="Miles" />
          miles
        </label>
        <button className="btn small" disabled={!q.trim()} onClick={() => void pick()}>
          Tick those
        </button>
      </div>
      {note && <span className="small muted">{note}</span>}
    </div>
  );
}
