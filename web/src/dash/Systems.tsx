// The running systems as their control channels describe them: sites and
// their decoding, neighbours not recorded yet, patches, DMR and NXDN carriers.

import { adjacentIdentity, formatMhz, reaches, roaming, sameSite, siteAddress, systemColor, systemWithChannel } from "../config.ts";
import { Light } from "../charts.tsx";
import { addSite, setNotice, type AppState } from "../controller.ts";
import { num } from "../fmt.ts";
import type { AdjacentSite, Config, DmrSiteStatus, NxdnSiteStatus, Patch, SiteIdentity, TalkgroupName } from "../protocol.ts";
import type { CcMark } from "../Waterfall.tsx";
import { K, recentAvg, runningSites, siteHealth, val, type DashSystem, type DetailedSite, type RunningSite } from "./data.ts";

export const hex = (v: number | null | undefined) => (v === null || v === undefined ? "—" : v.toString(16).toUpperCase());

/** A neighbouring site announced and not set up here: its identity, and the site announcing it. */
export interface Neighbour {
  a: AdjacentSite;
  id: SiteIdentity;
  from: RunningSite;
}

/**
 * The neighbouring sites `from` announce that aren't set up here: no
 * running site is one or is on its control channel, and no system has that
 * channel. Each once, by its address.
 */
export function neighbours(c: Config, from: RunningSite[], running: RunningSite[]): Neighbour[] {
  const out = new Map<string, Neighbour>();
  for (const x of from) {
    const type = c.systems.find((y) => y.shortName === x.shortName)?.type;
    for (const a of x.adjacent) {
      const id = adjacentIdentity(a, type);
      const k = siteAddress(id);
      const here = running.some((y) => sameSite(y.identity, id) || (y.controlChannelHz != null && Math.abs(y.controlChannelHz - a.freqHz) < 6_000));
      if (!out.has(k) && !here && !systemWithChannel(c, a.freqHz)) out.set(k, { a, id, from: x });
    }
  }
  return [...out.values()];
}

/** The control channels of the neighbouring sites not set up here that a source reaches: faint markers named by address. */
export function adjacentCcMarks(s: AppState): CcMark[] {
  if (!s.config) return [];
  const running = runningSites(s.status?.systems ?? []);
  const at = new Map<number, CcMark>();
  for (const { a, id, from } of neighbours(s.config, running, running)) {
    if (at.has(a.freqHz) || !s.sources.some((x) => reaches({ rateHz: x.rateHz, guardHz: x.settings.guardHz }, x.centerHz, a.freqHz))) continue;
    const address = siteAddress(id);
    at.set(a.freqHz, { hz: a.freqHz, label: address, color: systemColor(s.config, from.shortName), faint: true, title: `Adjacent site ${address} (${roaming(from.identity, id)}), announced by ${from.label}` });
  }
  return [...at.values()];
}

/** One marker per control channel frequency (sites sharing one share a marker), coloured by system. */
export function ccMarks(c: Config | null, sites: RunningSite[]): CcMark[] {
  const at = new Map<number, RunningSite[]>();
  for (const x of sites) if (x.controlChannelHz) at.set(x.controlChannelHz, [...(at.get(x.controlChannelHz) ?? []), x]);
  return [...at].map(([hz, xs]) => ({
    hz,
    label: sites.length > 1 ? xs.map((x) => x.label).join(" · ") : "CC",
    color: systemColor(c, xs[0].shortName),
    title: `${xs.map((x) => x.label).join(", ")} control channel`,
  }));
}

/** A system of several sites: a row each — its control channel, how it decodes, its health. */
export function SiteRows(props: { s: AppState; x: DashSystem }) {
  const { s, x } = props;
  if (x.sites.length < 2) return null;
  return (
    <div className="site-rows">
      {x.sites.map((site) => {
        const h = siteHealth(s, site);
        const good = recentAvg(K.cc(site, "good"), 60) ?? 0;
        const bad = recentAvg(K.cc(site, "bad"), 60) ?? 0;
        const hz = site.status?.controlChannelHz;
        return (
          <div key={site.name} className="site-row small" title={h.why}>
            <Light level={h.level} title={h.why} />
            <b>{site.name}</b>
            <span className="mono">{hz ? formatMhz(hz) : "hunting"}</span>
            <span className="mono muted">
              {num(val(s, K.cc(site, "good")))} msg/s{good + bad > 0 ? ` · ${num((100 * good) / (good + bad), 0)}%` : ""}
            </span>
          </div>
        );
      })}
    </div>
  );
}

/**
 * Neighbouring sites the control channels announce that aren't set up yet —
 * add one to record it too, as a site of its system (next Start). A
 * neighbouring system's (another System ID or WACN: one its radios roam
 * to) is added as a system of its own.
 */
export function Neighbours(props: { s: AppState; sites: RunningSite[] }) {
  const c = props.s.config;
  if (!c) return null;
  const kind = (n: Neighbour) => roaming(n.from.identity, n.id);
  const rows = neighbours(c, props.sites, runningSites(props.s.status?.systems ?? [])).sort((x, y) => Number(kind(x) !== "intra-system") - Number(kind(y) !== "intra-system"));
  if (!rows.length) return null;
  const add = ({ a, id }: Neighbour) => {
    const name = addSite([a.freqHz], id);
    setNotice(`Added ${name} (site ${siteAddress(id)}, ${formatMhz(a.freqHz)} MHz). It records from the next Start, if a source covers it.`);
  };
  const others = rows.filter((n) => kind(n) !== "intra-system").length;
  return (
    <details className="panel log">
      <summary className="panel-head">
        <h2>Neighbouring sites</h2>
        <span className="muted small">
          {rows.length} announced, not recorded{others ? ` · ${others} of other systems (inter-system or inter-WACN)` : ""}
        </span>
      </summary>
      <div className="row">
        {rows.map((n) => (
          <span key={siteAddress(n.id)} className="chip">
            <span className="mono">
              {siteAddress(n.id)} · {formatMhz(n.a.freqHz)}
            </span>
            {kind(n) !== "intra-system" && <span className="badge" title="Another System ID of the WACN (inter-system), or another WACN (inter-WACN): its radios roam here">{kind(n)}</span>}
            <span className="muted">via {n.from.label}</span>
            <button className="btn ghost small" onClick={() => add(n)}>
              Add
            </button>
          </span>
        ))}
      </div>
    </details>
  );
}

export const tgText = (t: TalkgroupName) => (t.alphaTag ? `${t.alphaTag} (${t.talkgroup})` : String(t.talkgroup));

/** The patches a system's control channels say stand now. A patched call is on its supergroup, usually not in the talkgroup file. */
export function Patches(props: { patches: Patch[] }) {
  const all = props.patches;
  if (!all.length) return null;
  return (
    <section className="panel">
      <header className="panel-head">
        <h2>Patches</h2>
        <span className="muted small">{all.length} active</span>
      </header>
      <div className="row">
        {all.map((p) => (
          <span key={p.supergroup.talkgroup} className="chip">
            <span className="mono">SG {tgText(p.supergroup)}</span>
            <span>= {p.members.map(tgText).join(" + ")}</span>
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
export function DmrSites(props: { sites: DetailedSite[] }) {
  const sites = props.sites.filter((x) => x.dmr);
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
          <div key={x.label} className="stack">
            <div className="row small">
              {props.sites.length > 1 && <b>{x.label}</b>}
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
export function NxdnSites(props: { sites: DetailedSite[] }) {
  const sites = props.sites.filter((x) => x.nxdn);
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
          <div key={x.label} className="stack">
            <div className="row small">
              {props.sites.length > 1 && <b>{x.label}</b>}
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
