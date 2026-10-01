import { Fragment, useEffect, useMemo, useRef, useState } from "react";
import { activeSystems, formatMhz, startProblem, systemColor, systemWithChannel } from "./config.ts";
import { addSite, dismissError, downloadCall, quitApp, setListen, setNotice, setView, start, stop, transport, useApp, web, type AppState } from "./controller.ts";
import { PluginsPage } from "./Plugins.tsx";
import { BrowserStorage } from "./web/BrowserStorage.tsx";
import { CONVENTIONAL, type CallEntry, type CallView, type DmrSiteStatus, type SystemStatus, type TalkgroupName } from "./protocol.ts";
import { Setup } from "./Setup.tsx";
import { parseTalkgroupCsv } from "./talkgroups.ts";
import { Waterfall, type CcMark } from "./Waterfall.tsx";

const hex = (v: number | null | undefined) => (v === null || v === undefined ? "—" : v.toString(16).toUpperCase());

function clock(s: number): string {
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = Math.floor(s % 60);
  return h ? `${h}:${String(m).padStart(2, "0")}:${String(ss).padStart(2, "0")}` : `${m}:${String(ss).padStart(2, "0")}`;
}

function Tile(props: { label: string; value: React.ReactNode; sub?: React.ReactNode; tone?: "ok" | "warn" | "bad" }) {
  return (
    <div className={`tile${props.tone ? ` tone-${props.tone}` : ""}`}>
      <div className="tile-label">{props.label}</div>
      <div className="tile-value">{props.value}</div>
      {props.sub && <div className="tile-sub">{props.sub}</div>}
    </div>
  );
}

/** One marker per control channel frequency (systems sharing one share a marker). */
function ccMarks(systems: SystemStatus[]): CcMark[] {
  const at = new Map<number, SystemStatus[]>();
  for (const x of systems) if (x.controlChannelHz) at.set(x.controlChannelHz, [...(at.get(x.controlChannelHz) ?? []), x]);
  return [...at].map(([hz, xs]) => ({ hz, label: systems.length > 1 ? xs.map((x) => x.shortName).join(" · ") : "CC", color: systemColor(xs[0].index) }));
}

/** Control channel messages per second of each system, over ≥2 s of its clock. */
function useMsgRates(systems: SystemStatus[]): Map<number, number> {
  const marks = useRef(new Map<number, { good: number; t: number; perS: number }>());
  const out = new Map<number, number>();
  for (const x of systems) {
    const prev = marks.current.get(x.index);
    if (!prev || x.nowS < prev.t) marks.current.set(x.index, { good: x.good, t: x.nowS, perS: 0 });
    else if (x.nowS - prev.t >= 2) marks.current.set(x.index, { good: x.good, t: x.nowS, perS: (x.good - prev.good) / (x.nowS - prev.t) });
    out.set(x.index, marks.current.get(x.index)!.perS);
  }
  return out;
}

/** A radio: its talker alias when known (the unit ID on hover), else its unit ID. */
function Unit({ id, alias }: { id: number; alias: string | undefined }) {
  return alias ? (
    <span className="unit" title={`Unit ${id}`}>
      {alias}
    </span>
  ) : (
    <span className="mono">{id}</span>
  );
}

/** A unit's alias on a system: as learned, else as the call record saved it. */
const aliasOf = (s: AppState, system: string, id: number, saved?: string) => s.units[system]?.[id] || saved || undefined;

const ccTone = (x: SystemStatus, perS: number | undefined): "ok" | "warn" | "bad" => (x.mismatch ? "bad" : (perS ?? 0) > 5 ? "ok" : (perS ?? 0) > 0 ? "warn" : "bad");
const pctText = (x: SystemStatus) => (x.good + x.bad ? `${Math.round((100 * x.good) / (x.good + x.bad))}% decoded` : "no decodes yet");

function StatusTiles({ s }: { s: AppState }) {
  const st = s.status;
  const systems = st?.systems ?? [];
  const rates = useMsgRates(systems);
  const srcTrouble = s.sources.some((x) => x.dropped > 0 || x.errors > 0 || (!x.ended && x.rateMeasured > 0 && Math.abs(x.rateMeasured / x.rateHz - 1) > 0.05));
  const maxRecorders = s.config?.recording.maxRecorders ?? 0;
  const trunked = s.config ? activeSystems(s.config).length > 0 : false;
  const convCount = (s.config?.conventional?.channels ?? []).filter((c) => c.enabled).length;
  const one = systems.length === 1 ? systems[0] : null;
  return (
    <>
      <div className="tiles">
        {convCount > 0 && (
          <Tile
            label="Conventional"
            value={
              <span className="mono">
                {st?.conventionalOpen ?? 0} <small>/ {convCount}</small>
              </span>
            }
            sub="channels with a signal now"
          />
        )}
        {one && (
          <Tile
            label={one.dmr?.variant === "DMR Capacity Plus" ? "Rest channel" : "Control channel"}
            tone={ccTone(one, rates.get(one.index))}
            value={one.controlChannelHz ? <span className="mono">{formatMhz(one.controlChannelHz)}</span> : "—"}
            sub={
              one.mismatch ? (
                `not this system: ${one.mismatch}`
              ) : (
                <>
                  {rates.get(one.index)?.toFixed(1) ?? "…"} msg/s · {pctText(one)} · {one.modulation ?? "detecting"}
                </>
              )
            }
          />
        )}
        {one && one.dmr && (
          <Tile
            label="System"
            value={<span>{one.dmr.variant?.replace(/^DMR /, "") ?? "DMR"}</span>}
            sub={<span className="mono">{dmrText(one.dmr)}</span>}
          />
        )}
        {one && !one.dmr && (
          <Tile
            label="System"
            value={<span className="mono">{one.identity.nac != null ? `NAC ${hex(one.identity.nac)}` : "—"}</span>}
            sub={
              <span className="mono">
                WACN {hex(one.identity.wacn)} · SysID {hex(one.identity.sysId)} · RFSS {one.identity.rfss ?? "—"} Site {one.identity.site ?? "—"}
              </span>
            }
          />
        )}
        {trunked ? (
          <Tile
            label="Recorders"
            value={
              <span className="mono">
                {st?.recording ?? 0} <small>/ {maxRecorders}</small>
              </span>
            }
            sub={`${st?.activeCalls ?? 0} active calls · ${st?.callsConcluded ?? 0} saved this run${systems.length > 1 ? ` · ${systems.length} systems` : ""}`}
          />
        ) : (
          <Tile label="Calls" value={<span className="mono">{st?.activeCalls ?? 0}</span>} sub={`active · ${st?.callsConcluded ?? 0} saved this run`} />
        )}
        <Tile
          label={s.sources.length > 1 ? `Radios (${s.sources.length})` : "Radio"}
          tone={!s.sources.length ? undefined : srcTrouble ? "warn" : "ok"}
          value={<span className="mono">{s.sources.length ? s.sources.map((x) => (x.rateMeasured / 1e6).toFixed(2)).join(" · ") + " MSPS" : "—"}</span>}
          sub={
            s.sources.length
              ? `DSP ${(s.load * 100).toFixed(1)}% of a core${s.sources.map((x) => (x.dropped ? ` · ${x.label}: ${x.dropped} samples dropped` : "") + (x.errors ? ` · ${x.label}: ${x.lastError}` : "")).join("")}`
              : "…"
          }
        />
        <Tile label="Uptime" value={<span className="mono">{st ? clock(st.nowS) : "0:00"}</span>} sub={s.sources.every((x) => x.label.startsWith("file")) && s.sources.length ? "replaying capture" : "live"} />
      </div>
      {systems.length > 1 && <SystemsTable s={s} systems={systems} rates={rates} />}
      {systems.length > 0 && <Neighbours s={s} systems={systems} />}
      {systems.length > 0 && <Patches systems={systems} />}
      {systems.some((x) => x.dmr) && <DmrSites systems={systems} />}
    </>
  );
}

/** "multi-site: 3 sites", or "3 systems on site 1-3" when they share one. */
function sitesText(g: SystemStatus[]): string {
  const sites = new Set(g.filter((x) => x.identity.site != null).map((x) => `${x.identity.rfss}-${x.identity.site}`));
  if (sites.size > 1) return `multi-site: ${sites.size} sites${sites.size < g.length ? `, ${g.length} systems` : ""}`;
  return `${g.length} systems${sites.size === 1 ? ` on site ${[...sites][0]}` : ""}`;
}

/** Every running system (site), sites of one system (same WACN / SysID) together. */
function SystemsTable(props: { s: AppState; systems: SystemStatus[]; rates: Map<number, number> }) {
  const { systems, rates } = props;
  const key = (x: SystemStatus) => (x.identity.wacn != null && x.identity.sysId != null ? `${x.identity.wacn}/${x.identity.sysId}` : `solo-${x.index}`);
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
                      WACN <span className="mono">{hex(g[0].identity.wacn)}</span> · SysID <span className="mono">{hex(g[0].identity.sysId)}</span> ·{" "}
                      {sitesText(g)}
                    </td>
                  </tr>
                )}
                {g.map((x) => {
                  const perS = rates.get(x.index);
                  const tone = ccTone(x, perS);
                  return (
                    <tr key={x.index} className={x.mismatch ? "mismatch" : ""}>
                      <td className="sys-name">
                        <span className="sys-dot" style={{ background: systemColor(x.index) }} />
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
function Neighbours(props: { s: AppState; systems: SystemStatus[] }) {
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

const tgText = (t: TalkgroupName) => (t.alphaTag ? `${t.alphaTag} (${t.talkgroup})` : String(t.talkgroup));

/** The patches the control channels say stand now. A patched call is on its supergroup, usually not in the talkgroup file. */
function Patches(props: { systems: SystemStatus[] }) {
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
          <span key={`${from.index}/${p.supergroup.talkgroup}`} className="chip">
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
function dmrText(d: DmrSiteStatus): string {
  const parts = [d.colorCode != null ? `CC ${d.colorCode}` : "CC ?"];
  if (d.rest) parts.push(`rest LSN ${d.rest.lsn}${d.rest.freqHz ? ` (${formatMhz(d.rest.freqHz)})` : ""}`);
  if (d.keyed) parts.push("keyed");
  return parts.join(" · ");
}

/** Each DMR site: its frequencies (which carries control, each slot's call now) and its channel table. */
function DmrSites(props: { systems: SystemStatus[] }) {
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
          <div key={x.index} className="stack">
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

/** Under a call's talkgroup: what's patched with it. */
function PatchedWith({ tgs }: { tgs: TalkgroupName[] }) {
  if (!tgs.length) return null;
  const text = tgs.map(tgText).join(", ");
  return (
    <div className="patched small" title={`Patched with ${text}`}>
      patched with {text}
    </div>
  );
}

function reasonText(c: CallView): string {
  if (c.state === "recording") return c.encrypted ? "recording (encrypted)" : "recording";
  switch (c.reason) {
    case "encrypted":
      return "encrypted — not recorded";
    case "no_source":
      return "outside tuned range";
    case "no_recorder":
      return "no free recorder";
    case "unknown_tg":
      return "not in talkgroup list";
    default:
      return "monitoring";
  }
}

/** The systems calls can come from: each running system, then conventional channels. */
function systemChoices(s: AppState): { index: number; name: string }[] {
  const out = (s.status?.systems ?? []).map((x) => ({ index: x.index, name: x.shortName }));
  const conv = s.config && s.config.conventional.channels.some((ch) => ch.enabled);
  if (conv && s.config) out.push({ index: CONVENTIONAL, name: s.config.conventional.shortName || "conventional" });
  return out;
}

/** Pick one system (or all): filter chips. */
function SystemFilter(props: { choices: { index: number; name: string }[]; value: number | null; onChange: (v: number | null) => void }) {
  if (props.choices.length < 2) return null;
  return (
    <div className="filter-row" role="radiogroup" aria-label="System">
      <button className={`btn ghost small${props.value === null ? " on" : ""}`} role="radio" aria-checked={props.value === null} onClick={() => props.onChange(null)}>
        All
      </button>
      {props.choices.map((x) => (
        <button
          key={x.index}
          className={`btn ghost small${props.value === x.index ? " on" : ""}`}
          role="radio"
          aria-checked={props.value === x.index}
          onClick={() => props.onChange(x.index)}
        >
          <span className="sys-dot" style={{ background: systemColor(x.index) }} />
          {x.name}
        </button>
      ))}
    </div>
  );
}

function ActiveCalls({ s }: { s: AppState }) {
  const now = s.status?.nowS ?? 0;
  const choices = systemChoices(s);
  const multi = choices.length > 1;
  const [only, setOnly] = useState<number | null>(null);
  const pick = (v: number | null) => {
    setOnly(v);
    // Live audio follows the filter.
    if (s.listen) setListen(true, v, null);
  };
  const calls = [...s.calls]
    .filter((c) => only === null || c.system === only)
    .sort((a, b) => Number(b.state === "recording") - Number(a.state === "recording") || b.startS - a.startS);
  const nameOf = (i: number | null) => choices.find((x) => x.index === i)?.name ?? "";
  const playingCall = s.nowPlaying && s.calls.find((c) => c.id === s.nowPlaying?.callId);
  const talkerId = playingCall?.sources.at(-1);
  const talker = playingCall && talkerId !== undefined ? { id: talkerId, alias: aliasOf(s, playingCall.systemName, talkerId) } : null;
  return (
    <section className="panel">
      <header className="panel-head">
        <h2>Active calls</h2>
        <div className="row">
          <label className="toggle">
            <input type="checkbox" checked={s.listen} onChange={(e) => setListen(e.target.checked, e.target.checked ? only : null, e.target.checked ? s.listenTalkgroup : null)} />
            <span>Listen live</span>
          </label>
          {s.listen && (s.listenTalkgroup !== null || s.listenSystem !== null) && (
            <button className="btn ghost" onClick={() => setListen(true, null, null)}>
              {[s.listenSystem !== null && multi ? nameOf(s.listenSystem) : "", s.listenTalkgroup !== null ? `TG ${s.listenTalkgroup}` : ""].filter(Boolean).join(" ")} only ✕
            </button>
          )}
        </div>
      </header>
      <SystemFilter choices={choices} value={only} onChange={pick} />
      {s.nowPlaying && s.listen && (
        <div className="now-playing">
          ▶ {multi ? `${nameOf(s.nowPlaying.system)} · ` : ""}TG {s.nowPlaying.talkgroup}
          {talker && (
            <>
              {" · "}
              <Unit id={talker.id} alias={talker.alias} />
            </>
          )}
        </div>
      )}
      {calls.length === 0 ? (
        <p className="empty">No calls right now.</p>
      ) : (
        <div className="table-wrap">
          <table className="calls">
            <thead>
              <tr>
                {multi && <th>System</th>}
                <th>Talkgroup</th>
                <th>Freq, MHz</th>
                <th>Source</th>
                <th>Time</th>
                <th>State</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {calls.map((c) => (
                <tr key={c.id} className={`st-${c.state}${c.emergency ? " emergency" : ""}`}>
                  {multi && (
                    <td className="sys-name">
                      <span className="sys-dot" style={{ background: systemColor(c.system) }} />
                      {c.systemName}
                    </td>
                  )}
                  <td>
                    <span className="tg">{c.talkgroup}</span>
                    {c.alphaTag && <span className="tag">{c.alphaTag}</span>}
                    {c.emergency && <span className="badge bad">EMERG</span>}
                    <PatchedWith tgs={c.patched ?? []} />
                  </td>
                  <td className="mono">
                    {formatMhz(c.freqHz, 4)}
                    {c.slot !== null && <span className="muted"> · s{c.slot}</span>}
                    {c.analog && <span className="muted"> · FM</span>}
                  </td>
                  <td className="unit-cell">{c.sources.length ? <Unit id={c.sources.at(-1)!} alias={aliasOf(s, c.systemName, c.sources.at(-1)!)} /> : "—"}</td>
                  <td className="mono">{clock(Math.max(0, now - c.startS))}</td>
                  <td>
                    <span className={`dot dot-${c.state}${c.encrypted ? " dot-enc" : ""}`} /> {reasonText(c)}
                  </td>
                  <td>
                    {c.state === "recording" && (
                      <button className="btn ghost small" onClick={() => setListen(true, c.system, c.talkgroup)} title="Listen to this talkgroup only">
                        Listen
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

/** A recorded call's system: its record's short_name, else its folder. */
const systemOf = (c: CallEntry) => c.record.short_name || c.path.split("/")[0] || "";

function History({ s }: { s: AppState }) {
  const [playing, setPlaying] = useState<string | null>(null);
  const [playingUrl, setPlayingUrl] = useState<string | null>(null);
  useEffect(() => {
    setPlayingUrl(null);
    if (!playing) return;
    let live = true;
    transport.callUrl(playing, "wav").then(
      (u) => live && setPlayingUrl(u),
      () => live && setPlaying(null),
    );
    return () => {
      live = false;
    };
  }, [playing]);
  const [filter, setFilter] = useState("");
  const [only, setOnly] = useState<string | null>(null);
  const systems = useMemo(() => [...new Set(s.history.map(systemOf))].sort(), [s.history]);
  // Each system's talkgroup file, by short name: names for a call's patched talkgroups.
  const files = useMemo(() => new Map((s.config?.systems ?? []).map((x) => [x.shortName, parseTalkgroupCsv(x.talkgroupsCsv)])), [s.config]);
  const patchedWith = (c: CallEntry): TalkgroupName[] =>
    (c.record.patched_talkgroups ?? [])
      .filter((t) => t !== c.record.talkgroup)
      .map((t) => ({ talkgroup: t, alphaTag: files.get(systemOf(c))?.get(t)?.alphaTag ?? "" }));
  const rows = useMemo(() => {
    const f = filter.trim().toLowerCase();
    const unitMatch = (c: CallEntry) =>
      c.record.srcList?.some((x) => String(x.src).includes(f) || (aliasOf(s, systemOf(c), x.src, x.tag_ota) ?? "").toLowerCase().includes(f));
    const ok = (c: CallEntry) =>
      (only === null || systemOf(c) === only) &&
      (!f ||
        String(c.record.talkgroup).includes(f) ||
        (c.record.talkgroup_tag ?? "").toLowerCase().includes(f) ||
        patchedWith(c).some((t) => String(t.talkgroup).includes(f) || t.alphaTag.toLowerCase().includes(f)) ||
        unitMatch(c));
    return f || only !== null ? s.history.filter(ok) : s.history;
  }, [s.history, s.units, filter, only, files]);
  const multi = systems.length > 1;
  // A system's color while it runs (its index), else none.
  const colorOf = (name: string) => {
    const i = s.status?.systems.find((x) => x.shortName === name)?.index;
    return i === undefined ? "var(--line)" : systemColor(i);
  };

  return (
    <section className="panel">
      <header className="panel-head">
        <h2>Recent calls</h2>
        <div className="row">
          {multi && (
            <select value={only ?? ""} onChange={(e) => setOnly(e.target.value || null)} aria-label="System">
              <option value="">All systems</option>
              {systems.map((x) => (
                <option key={x} value={x}>
                  {x}
                </option>
              ))}
            </select>
          )}
          <input className="search" placeholder="Filter talkgroup or unit…" value={filter} onChange={(e) => setFilter(e.target.value)} />
        </div>
      </header>
      {rows.length === 0 ? (
        <p className="empty">Recorded calls will appear here.</p>
      ) : (
        <div className="table-wrap history">
          <table className="calls">
            <thead>
              <tr>
                <th>Time</th>
                {multi && <th>System</th>}
                <th>Talkgroup</th>
                <th>Length</th>
                <th>Sources</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {rows.slice(0, 200).map((c) => (
                <tr key={c.path} className={playing === c.path ? "playing" : ""}>
                  <td className="mono">{new Date(c.record.start_time_ms).toLocaleTimeString()}</td>
                  {multi && (
                    <td className="sys-name small">
                      <span className="sys-dot" style={{ background: colorOf(systemOf(c)) }} />
                      {systemOf(c)}
                    </td>
                  )}
                  <td>
                    <span className="tg">{c.record.talkgroup}</span>
                    {c.record.talkgroup_tag && <span className="tag">{c.record.talkgroup_tag}</span>}
                    {c.record.emergency ? <span className="badge bad">EMERG</span> : null}
                    <PatchedWith tgs={patchedWith(c)} />
                  </td>
                  <td className="mono">{(c.record.call_length_ms / 1000).toFixed(1)} s</td>
                  <Sources s={s} entry={c} />
                  <td className="actions">
                    <button className="btn ghost small" onClick={() => setPlaying(c.path)}>
                      Play
                    </button>
                    <button className="btn ghost small" onClick={() => void downloadCall(c.path, "wav")}>
                      WAV
                    </button>
                    <button className="btn ghost small" onClick={() => void downloadCall(c.path, "json")}>
                      JSON
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {playing && playingUrl && <audio className="player" src={playingUrl} controls autoPlay onEnded={() => setPlaying(null)} />}
      <footer className="panel-foot muted small">
        {web ? (
          <BrowserStorage />
        ) : s.config ? (
          <span>
            Saved to <span className="mono">{s.config.recording.captureDir}</span>
          </span>
        ) : null}
      </footer>
    </section>
  );
}

/** A recorded call's radios, in the order they spoke: aliases first, every one on hover. */
function Sources({ s, entry }: { s: AppState; entry: CallEntry }) {
  const system = systemOf(entry);
  const seen = new Map<number, string | undefined>();
  for (const x of entry.record.srcList ?? []) if (!seen.has(x.src)) seen.set(x.src, aliasOf(s, system, x.src, x.tag_ota));
  const units = [...seen];
  return (
    <td className="srcs" title={units.map(([id, alias]) => (alias ? `${alias} (${id})` : String(id))).join(", ")}>
      {units.length
        ? units.map(([id, alias], i) => (
            <Fragment key={id}>
              {i > 0 && ", "}
              <Unit id={id} alias={alias} />
            </Fragment>
          ))
        : "—"}
    </td>
  );
}

function Log({ s }: { s: AppState }) {
  const [show, setShow] = useState<"calls" | "all">("calls");
  const [only, setOnly] = useState<number | null>(null);
  const systems = s.status?.systems ?? [];
  const multi = systems.length > 1;
  const onlyName = systems.find((x) => x.index === only)?.shortName;
  const lines = (show === "all" ? s.log : s.log.filter((l) => /grant|update|control|patch|status|sysid|adjacent|error|alias|plugin/.test(l.kind))).filter(
    (l) => onlyName === undefined || l.system === onlyName,
  );
  return (
    <details className="panel log">
      <summary className="panel-head">
        <h2>Control channel log</h2>
        <span className="muted small">{s.log.length} recent messages</span>
      </summary>
      <div className="row log-filter">
        <label className="toggle">
          <input type="checkbox" checked={show === "all"} onChange={(e) => setShow(e.target.checked ? "all" : "calls")} />
          <span>Show unit activity (affiliations, registrations…)</span>
        </label>
        {multi && <SystemFilter choices={systems.map((x) => ({ index: x.index, name: x.shortName }))} value={only} onChange={setOnly} />}
      </div>
      <pre className="log-lines">
        {lines
          .slice(-150)
          .reverse()
          .map((l, i) => (
            <div key={i} className={`k-${l.kind}`}>
              <span className="muted">{l.timeS.toFixed(1).padStart(7)}s</span> {multi && l.system ? <span className="muted">[{l.system}] </span> : null}
              {l.text}
            </div>
          ))}
      </pre>
    </details>
  );
}

/** Enabled plugins that can't run or report a problem. */
function pluginTrouble(s: AppState): number {
  return (s.plugins?.plugins ?? []).filter((p) => p.enabled && (p.problem || p.runtime.state === "error" || p.runtime.state === "warning")).length;
}

export function App() {
  const s = useApp();
  const running = s.phase === "running" || s.phase === "starting";
  const problem = s.config ? startProblem(s.config) : "Connecting to the recorder…";
  const liveDongle = s.config?.sources.some((x) => x.kind === "rtlsdr") ?? false;

  if (s.quit) {
    return (
      <div className="app">
        <div className="quit-screen">
          <span className="logo" aria-hidden="true" />
          <h1>Trunk Recorder Pro has quit</h1>
          <p className="muted">Recording stopped and calls in progress were saved. You can close this tab; open the app again to start it.</p>
        </div>
      </div>
    );
  }

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <span className="logo" aria-hidden="true" />
          <div>
            <h1>Trunk Recorder Pro</h1>
            <p className="muted small">
              P25, SmartNet and DMR recorder{s.version ? ` · v${s.version}` : ""}
            </p>
          </div>
        </div>
        {!web && (
          <nav className="tabs" aria-label="Pages">
            <button className={s.view === "recorder" ? "on" : ""} aria-current={s.view === "recorder" ? "page" : undefined} onClick={() => setView("recorder")}>
              Recorder
            </button>
            <button className={s.view === "plugins" ? "on" : ""} aria-current={s.view === "plugins" ? "page" : undefined} onClick={() => setView("plugins")}>
              Plugins
              {pluginTrouble(s) > 0 && (
                <span className="tab-badge" title="A plugin needs attention">
                  {pluginTrouble(s)}
                </span>
              )}
            </button>
          </nav>
        )}
        <div className="row">
          <span className={`pill pill-${s.phase}`}>
            {!s.connected ? "Disconnected" : s.phase === "running" ? (liveDongle ? "Recording" : "Replaying") : s.phase === "idle" ? "Stopped" : s.phase === "starting" ? "Starting…" : "Stopping…"}
          </span>
          {running ? (
            <button className="btn primary" onClick={stop}>
              Stop
            </button>
          ) : (
            <button className="btn primary" disabled={!s.connected || s.phase === "stopping" || !!problem} title={problem ?? ""} onClick={start}>
              Start
            </button>
          )}
          {!web && s.connected && (
            <button className="btn ghost" onClick={quitApp} title="Stop recording and quit the app">
              Quit
            </button>
          )}
        </div>
      </header>

      {!s.connected && <div className="banner bad">Not connected to the recorder — is trunk-pro running? Retrying…</div>}
      {s.error && (
        <div className="banner bad" role="alert">
          <span>{s.error}</span>
          <button className="btn ghost small" onClick={dismissError}>
            Dismiss
          </button>
        </div>
      )}
      {s.notice && (
        <div className="banner" role="status">
          <span>{s.notice}</span>
          <button className="btn ghost small" onClick={() => setNotice(null)}>
            OK
          </button>
        </div>
      )}
      {s.ended && s.phase === "idle" && <div className="banner">Replay finished.</div>}
      {!running && problem && s.connected && !s.error && <div className="banner subtle">{problem}</div>}

      <main>
        {!web && s.view === "plugins" ? (
          <PluginsPage />
        ) : running ? (
          <>
            <StatusTiles s={s} />
            {s.spectra.map((sp, i) =>
              sp ? (
                <section className="panel flush" key={i}>
                  <Waterfall
                    radio={sp}
                    label={s.sources[i]?.label}
                    ccs={ccMarks(s.status?.systems ?? [])}
                    calls={s.calls}
                    multi={systemChoices(s).length > 1}
                  />
                </section>
              ) : null,
            )}
            <div className="columns">
              <ActiveCalls s={s} />
              <History s={s} />
            </div>
            <Log s={s} />
          </>
        ) : (
          <div className="columns setup-cols">
            <Setup />
            <History s={s} />
          </div>
        )}
      </main>
    </div>
  );
}
