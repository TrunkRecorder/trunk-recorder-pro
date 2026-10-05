// The running systems as their control channels describe them: sites and
// their decoding, neighbours not recorded yet, patches, DMR and NXDN carriers.

import { formatMhz, systemColor, systemWithChannel } from "../config.ts";
import { addSite, setNotice, type AppState } from "../controller.ts";
import type { Config, DmrSiteStatus, NxdnSiteStatus, SystemStatus, TalkgroupName } from "../protocol.ts";
import type { CcMark } from "../Waterfall.tsx";

export const hex = (v: number | null | undefined) => (v === null || v === undefined ? "—" : v.toString(16).toUpperCase());

/** One marker per control channel frequency (systems sharing one share a marker). */
export function ccMarks(c: Config | null, systems: SystemStatus[]): CcMark[] {
  const at = new Map<number, SystemStatus[]>();
  for (const x of systems) if (x.controlChannelHz) at.set(x.controlChannelHz, [...(at.get(x.controlChannelHz) ?? []), x]);
  return [...at].map(([hz, xs]) => ({ hz, label: systems.length > 1 ? xs.map((x) => x.shortName).join(" · ") : "CC", color: systemColor(c, xs[0].shortName) }));
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
              <span>{d.variant ?? "type unknown"}</span>
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

/** "NXDN48 · SysID 123 · site 1 · RAN 5 · DFA 451.0000 + n × 6.25 kHz". */
export function nxdnText(d: NxdnSiteStatus): string {
  const parts = [d.rate ? d.rate.toUpperCase() : "rate ?"];
  if (d.location) parts.push(`SysID ${hex(d.location.system)} · site ${d.location.site}`);
  parts.push(d.ran != null ? `RAN ${d.ran}` : "RAN ?");
  if (d.dfa) parts.push(`DFA ${formatMhz(d.dfa.baseHz)} + n × ${d.dfa.stepHz / 1000} kHz`);
  return parts.join(" · ");
}

/** Each NXDN site: its watched frequencies (control, repeater number, the call now), its channel table and the channels granted but not placed. */
export function NxdnSites(props: { systems: SystemStatus[] }) {
  const sites = props.systems.filter((x) => x.nxdn);
  return (
    <section className="panel">
      <header className="panel-head">
        <h2>NXDN</h2>
      </header>
      {sites.map((x) => {
        const d = x.nxdn!;
        const typeD = d.kind === "NXDN Type-D";
        const offTable = d.channels.filter((e) => !d.carriers.some((c) => c.freqHz === e.freqHz));
        return (
          <div key={x.shortName} className="stack">
            <div className="row small">
              {props.systems.length > 1 && <b>{x.shortName}</b>}
              <span>{d.kind}</span>
              <span className="mono">{nxdnText(d)}</span>
            </div>
            <div className="table-wrap">
              <table className="calls">
                <thead>
                  <tr>
                    <th>MHz</th>
                    <th>{typeD ? "Repeater" : "Channel"}</th>
                    <th>Call</th>
                  </tr>
                </thead>
                <tbody>
                  {d.carriers.map((c) => {
                    const ch = d.channels.find((e) => e.freqHz === c.freqHz);
                    const num = typeD && c.repeater != null ? String(c.repeater) : ch ? `${ch.channel}${ch.configured ? "" : " (learned)"}` : "—";
                    return (
                      <tr key={c.freqHz}>
                        <td className="mono">
                          {formatMhz(c.freqHz)}
                          {c.control && <span className="chip ok small">control</span>}
                          {c.ran != null && d.ran != null && c.ran !== d.ran && <span className="chip warn small">RAN {c.ran}</span>}
                        </td>
                        <td className="mono small">{num}</td>
                        <td className="small">
                          {c.call ? (
                            <span>
                              {tgText(c.call.talkgroup)}
                              {c.call.source ? <span className="muted"> · {c.call.source}</span> : null}
                            </span>
                          ) : (
                            <span className="muted">—</span>
                          )}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
            {offTable.length > 0 && (
              <div className="row small">
                <span className="muted">Also in the channel table:</span>
                {offTable.map((e) => (
                  <span key={e.channel} className="chip mono">
                    {e.channel} → {formatMhz(e.freqHz)}
                    {e.configured ? "" : " (learned)"}
                  </span>
                ))}
              </div>
            )}
            {d.unknownChannels.length > 0 && (
              <p className="why why-warn small">
                {d.unknownChannels.length === 1
                  ? `Channel ${d.unknownChannels[0]} was granted but isn't in the channel table — it is learned when its call shows up on a listed voice frequency; or add it to the channel table in Setup.`
                  : `Channels ${d.unknownChannels.join(", ")} were granted but aren't in the channel table — each is learned when its call shows up on a listed voice frequency; or add them to the channel table in Setup.`}
              </p>
            )}
          </div>
        );
      })}
    </section>
  );
}
