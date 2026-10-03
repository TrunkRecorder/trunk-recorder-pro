// The running systems as their control channels describe them: sites and
// their decoding, neighbours not recorded yet, patches, DMR carriers.

import { Fragment, useRef } from "react";
import { formatMhz, systemColor, systemWithChannel } from "../config.ts";
import { addSite, setNotice, type AppState } from "../controller.ts";
import type { Config, DmrSiteStatus, SystemStatus, TalkgroupName } from "../protocol.ts";
import type { CcMark } from "../Waterfall.tsx";

export const hex = (v: number | null | undefined) => (v === null || v === undefined ? "—" : v.toString(16).toUpperCase());

/** One marker per control channel frequency (systems sharing one share a marker). */
export function ccMarks(c: Config | null, systems: SystemStatus[]): CcMark[] {
  const at = new Map<number, SystemStatus[]>();
  for (const x of systems) if (x.controlChannelHz) at.set(x.controlChannelHz, [...(at.get(x.controlChannelHz) ?? []), x]);
  return [...at].map(([hz, xs]) => ({ hz, label: systems.length > 1 ? xs.map((x) => x.shortName).join(" · ") : "CC", color: systemColor(c, xs[0].shortName) }));
}

/** Control channel messages per second of each system (by short name), over ≥2 s of its clock. */
export function useMsgRates(systems: SystemStatus[]): Map<string, number> {
  const marks = useRef(new Map<string, { good: number; t: number; perS: number }>());
  const out = new Map<string, number>();
  for (const x of systems) {
    const prev = marks.current.get(x.shortName);
    if (!prev || x.nowS < prev.t) marks.current.set(x.shortName, { good: x.good, t: x.nowS, perS: 0 });
    else if (x.nowS - prev.t >= 2) marks.current.set(x.shortName, { good: x.good, t: x.nowS, perS: (x.good - prev.good) / (x.nowS - prev.t) });
    out.set(x.shortName, marks.current.get(x.shortName)!.perS);
  }
  return out;
}

const ccTone = (x: SystemStatus, perS: number | undefined): "ok" | "warn" | "bad" => (x.mismatch ? "bad" : (perS ?? 0) > 5 ? "ok" : (perS ?? 0) > 0 ? "warn" : "bad");
const pctText = (x: SystemStatus) => (x.good + x.bad ? `${Math.round((100 * x.good) / (x.good + x.bad))}% decoded` : "no decodes yet");

/** "multi-site: 3 sites", or "3 systems on site 1-3" when they share one. */
function sitesText(g: SystemStatus[]): string {
  const sites = new Set(g.filter((x) => x.identity.site != null).map((x) => `${x.identity.rfss}-${x.identity.site}`));
  if (sites.size > 1) return `multi-site: ${sites.size} sites${sites.size < g.length ? `, ${g.length} systems` : ""}`;
  return `${g.length} systems${sites.size === 1 ? ` on site ${[...sites][0]}` : ""}`;
}

/** Every running system (site), sites of one system (same WACN / SysID) together. */
export function SystemsTable(props: { s: AppState; systems: SystemStatus[]; rates: Map<string, number> }) {
  const { systems, rates } = props;
  const key = (x: SystemStatus) => x.siteGroup ?? (x.identity.wacn != null && x.identity.sysId != null ? `${x.identity.wacn}/${x.identity.sysId}` : `solo-${x.shortName}`);
  const groups = new Map<string, SystemStatus[]>();
  for (const x of systems) groups.set(key(x), [...(groups.get(key(x)) ?? []), x]);
  return (
    <section className="panel">
      <header className="panel-head">
        <h2>Systems</h2>
        <span className="muted small">
          {systems.length} following their control channels · recorders shared ({props.s.status?.recording ?? 0} in use)
        </span>
      </header>
      <div className="table-wrap">
        <table className="calls sys-table">
          <thead>
            <tr>
              <th>System</th>
              <th>Site</th>
              <th>Control channel</th>
              <th>Decoding</th>
              <th>NAC</th>
              <th>Calls</th>
            </tr>
          </thead>
          <tbody>
            {[...groups.values()].map((g) => (
              <Fragment key={key(g[0])}>
                {g.length > 1 && (
                  <tr className="group-head">
                    <td colSpan={6}>
                      {g[0].siteGroup?.startsWith("group:") ? (
                        <>
                          Site group <b>{g[0].siteGroup.slice(6)}</b>
                        </>
                      ) : (
                        <>
                          WACN <span className="mono">{hex(g[0].identity.wacn)}</span> · SysID <span className="mono">{hex(g[0].identity.sysId)}</span>
                        </>
                      )}{" "}
                      · {sitesText(g)}
                    </td>
                  </tr>
                )}
                {g.map((x) => {
                  const perS = rates.get(x.shortName);
                  const tone = ccTone(x, perS);
                  return (
                    <tr key={x.shortName} className={x.mismatch ? "mismatch" : ""}>
                      <td className="sys-name">
                        <span className="sys-dot" style={{ background: systemColor(props.s.config, x.shortName) }} />
                        <b>{x.shortName}</b>
                      </td>
                      <td className={x.dmr ? "small" : "mono"}>
                        {x.dmr ? (x.dmr.variant?.replace(/^DMR /, "") ?? "DMR") : x.identity.site != null ? `${x.identity.rfss ?? "?"}-${x.identity.site}` : "—"}
                      </td>
                      <td className="mono">
                        <span className={`dot dot-${tone === "ok" ? "recording" : "monitoring"}`} /> {x.controlChannelHz ? formatMhz(x.controlChannelHz) : "—"}
                        {x.mismatch && <div className="small">not this system: {x.mismatch}</div>}
                      </td>
                      <td className="small">
                        <span className={`chip ${tone}`}>{perS !== undefined ? `${perS.toFixed(1)} msg/s` : "…"}</span> {pctText(x)}
                        {x.modulation ? ` · ${x.modulation}` : ""}
                      </td>
                      <td className="mono">{x.dmr ? (x.dmr.colorCode != null ? `CC ${x.dmr.colorCode}` : "CC ?") : hex(x.identity.nac)}</td>
                      <td className="mono small">
                        {x.activeCalls} active · {x.recording} rec · {x.callsConcluded} saved
                      </td>
                    </tr>
                  );
                })}
              </Fragment>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}

/** Neighbouring sites the control channels announce that aren't set up yet — add one to record it too (next Start). */
export function Neighbours(props: { s: AppState; systems: SystemStatus[] }) {
  const c = props.s.config;
  if (!c) return null;
  const seen = new Map<string, { a: SystemStatus["adjacent"][number]; from: SystemStatus }>();
  for (const x of props.systems)
    for (const a of x.adjacent) {
      const k = `${a.sysId}/${a.rfss}/${a.site}`;
      const recorded = props.systems.some((y) => y.identity.sysId === a.sysId && y.identity.rfss === a.rfss && y.identity.site === a.site);
      if (!recorded && !systemWithChannel(c, a.freqHz) && !seen.has(k)) seen.set(k, { a, from: x });
    }
  if (!seen.size) return null;
  const add = (a: SystemStatus["adjacent"][number], from: SystemStatus) => {
    const name = addSite([a.freqHz], { wacn: from.identity.wacn, sysId: a.sysId, rfss: a.rfss, site: a.site });
    setNotice(`Added ${name} (site ${a.rfss}-${a.site}, ${formatMhz(a.freqHz)} MHz). It records from the next Start, if a source covers it.`);
  };
  return (
    <details className="panel log">
      <summary className="panel-head">
        <h2>Neighbouring sites</h2>
        <span className="muted small">{seen.size} announced, not recorded</span>
      </summary>
      <div className="row">
        {[...seen.values()].map(({ a, from }) => (
          <span key={`${a.sysId}/${a.rfss}/${a.site}`} className="chip">
            <span className="mono">
              SysID {hex(a.sysId)} · site {a.rfss}-{a.site} · {formatMhz(a.freqHz)}
            </span>
            <span className="muted">via {from.shortName}</span>
            <button className="btn ghost small" onClick={() => add(a, from)}>
              Add
            </button>
          </span>
        ))}
      </div>
    </details>
  );
}

export const tgText = (t: TalkgroupName) => (t.alphaTag ? `${t.alphaTag} (${t.talkgroup})` : String(t.talkgroup));

/** The patches the control channels say stand now. A patched call is on its supergroup, usually not in the talkgroup file. */
export function Patches(props: { systems: SystemStatus[] }) {
  const all = props.systems.flatMap((x) => (x.patches ?? []).map((p) => ({ p, from: x })));
  if (!all.length) return null;
  return (
    <section className="panel">
      <header className="panel-head">
        <h2>Patches</h2>
        <span className="muted small">{all.length} active</span>
      </header>
      <div className="row">
        {all.map(({ p, from }) => (
          <span key={`${from.shortName}/${p.supergroup.talkgroup}`} className="chip">
            <span className="mono">SG {tgText(p.supergroup)}</span>
            <span>= {p.members.map(tgText).join(" + ")}</span>
            {props.systems.length > 1 && <span className="muted">· {from.shortName}</span>}
          </span>
        ))}
      </div>
    </section>
  );
}

/** "CC 14 · rest LSN 2 (463.3750) · keyed". */
export function dmrText(d: DmrSiteStatus): string {
  const parts = [d.colorCode != null ? `CC ${d.colorCode}` : "CC ?"];
  if (d.rest) parts.push(`rest LSN ${d.rest.lsn}${d.rest.freqHz ? ` (${formatMhz(d.rest.freqHz)})` : ""}`);
  if (d.keyed) parts.push("keyed");
  return parts.join(" · ");
}

/** Each DMR site: its frequencies (which carries control, each slot's call now) and its channel table. */
export function DmrSites(props: { systems: SystemStatus[] }) {
  const sites = props.systems.filter((x) => x.dmr);
  return (
    <section className="panel">
      <header className="panel-head">
        <h2>DMR</h2>
        <span className="muted small">every listed frequency is watched; the channel table is learned from the air</span>
      </header>
      {sites.map((x) => {
        const d = x.dmr!;
        const slot = (s: { talkgroup: TalkgroupName; source: number } | null | undefined) =>
          s ? (
            <span>
              {tgText(s.talkgroup)}
              {s.source ? <span className="muted"> · {s.source}</span> : null}
            </span>
          ) : (
            <span className="muted">—</span>
          );
        return (
          <div key={x.shortName} className="stack">
            <div className="row small">
              {props.systems.length > 1 && <b>{x.shortName}</b>}
              <span>{d.variant ?? "kind not known yet"}</span>
              <span className="mono">{dmrText(d)}</span>
            </div>
            <div className="table-wrap">
              <table className="calls">
                <thead>
                  <tr>
                    <th>MHz</th>
                    <th>Channel</th>
                    <th>Slot 1</th>
                    <th>Slot 2</th>
                  </tr>
                </thead>
                <tbody>
                  {d.carriers.map((c) => {
                    const lcn = d.channels.find((e) => e.freqHz === c.freqHz);
                    return (
                      <tr key={c.freqHz}>
                        <td className="mono">
                          {formatMhz(c.freqHz)}
                          {c.control && <span className="chip ok small">{d.variant === "DMR Capacity Plus" ? "rest" : "control"}</span>}
                          {c.colorCode != null && d.colorCode != null && c.colorCode !== d.colorCode && <span className="chip warn small">CC {c.colorCode}</span>}
                        </td>
                        <td className="mono small">{lcn ? `${lcn.lcn}${lcn.configured ? "" : " (learned)"}` : "—"}</td>
                        <td className="small">{slot(c.slots[0])}</td>
                        <td className="small">{slot(c.slots[1])}</td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
            {d.channels.some((e) => !d.carriers.some((c) => c.freqHz === e.freqHz)) && (
              <div className="row small">
                <span className="muted">Also in the channel table:</span>
                {d.channels
                  .filter((e) => !d.carriers.some((c) => c.freqHz === e.freqHz))
                  .map((e) => (
                    <span key={e.lcn} className="chip mono">
                      {e.lcn} → {formatMhz(e.freqHz)}
                      {e.configured ? "" : " (learned)"}
                    </span>
                  ))}
              </div>
            )}
          </div>
        );
      })}
    </section>
  );
}
