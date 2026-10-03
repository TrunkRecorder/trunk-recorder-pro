// The radio system as heard: which talkgroups are busy (and which are new,
// unknown, or ignored), which radios are on the air, what they're
// affiliated with and who they talk with — and, for one talkgroup or one
// radio, its week and its connections.

import { useMemo, useState } from "react";
import { systemColor } from "../config.ts";
import { radioQuery, setTalkgroupIgnore, setView, usePolled, useApp, type AppState } from "../controller.ts";
import { Bipartite, Card, Choice, Heatmap, HeatStrip, Hint, Histogram, Light, RadialEgo, Stat, type Node } from "../charts.tsx";
import { ago, clockAt, compact, dur, hmm, num } from "../fmt.ts";
import type { LengthHistogram, RadioResult, TgRow, UnitRow } from "../protocol.ts";
import { aliasOf } from "./Calls.tsx";
import { dashSystems, K, running, total, useHistory, type DashSystem } from "./data.ts";

type Hours = "24" | "168";
const HOURS: { v: Hours; label: string }[] = [
  { v: "24", label: "24 h" },
  { v: "168", label: "7 days" },
];

/** Why calls weren't recorded (and were), in the order shown. */
const REASONS: { k: string; label: string; color: string; hint: string }[] = [
  { k: "recorded", label: "Recorded", color: "var(--series-1)", hint: "" },
  { k: "monitored", label: "Followed only", color: "var(--series-3)", hint: "followed (to learn who talks) but not recorded" },
  { k: "ignored", label: "Ignored", color: "var(--muted-mark)", hint: "the talkgroup file says not to record" },
  { k: "encrypted", label: "Encrypted", color: "var(--series-7)", hint: "encrypted, and set not to record encrypted calls" },
  { k: "unknown_tg", label: "Not in the file", color: "var(--series-4)", hint: "not in the talkgroup file, and unknown talkgroups are off" },
  { k: "no_recorder", label: "No recorder", color: "var(--critical)", hint: "every recorder busy: raise the maximum" },
  { k: "no_source", label: "Out of band", color: "var(--serious)", hint: "no source covers the frequency: move a centre" },
];

const hourLabel = (hours: number) => (i: number) => {
  const t = (Math.floor(Date.now() / 3600_000) - (hours - 1 - i)) * 3600;
  return clockAt(t);
};

function TgName({ r }: { r: { tg: number; alphaTag: string } }) {
  return r.alphaTag ? (
    <span className="tg-name" title={`Talkgroup ${r.tg}`}>
      {r.alphaTag}
    </span>
  ) : (
    <span className="tg">{r.tg}</span>
  );
}

function IgnoreToggle({ sys, r }: { sys: DashSystem; r: TgRow }) {
  if (sys.kind === "conventional") return null;
  return (
    <button
      className={`btn ghost small${r.ignore ? " on" : ""}`}
      onClick={(e) => {
        e.stopPropagation();
        setTalkgroupIgnore(sys.name, r.tg, !r.ignore, r.alphaTag);
      }}
      title={r.ignore ? "Record it again (clears Ignore in the talkgroup file)" : "Never record it (sets Ignore in the talkgroup file; takes effect now)"}
    >
      {r.ignore ? "Ignored ✕" : "Ignore"}
    </button>
  );
}

// ── the system ───────────────────────────────────────────────────────────────

function Reasons({ name, range }: { name: string; range: "24h" | "7d" }) {
  const hist = useHistory(REASONS.map((r) => K.sys(name, `why/${r.k}`)), range, 48);
  const counts = REASONS.map((r) => Math.round(total(hist?.[K.sys(name, `why/${r.k}`)]) ?? 0));
  const sum = counts.reduce((a, b) => a + b, 0);
  return (
    <Card title="What happened to calls">
      {sum === 0 ? (
        <p className="empty">Nothing yet in this span.</p>
      ) : (
        <>
          <div className="stackbar" role="img" aria-label="calls by what happened">
            {REASONS.map((r, i) =>
              counts[i] > 0 ? <span key={r.k} style={{ flexGrow: counts[i], background: r.color }} title={`${r.label}: ${Math.round(counts[i])} (${Math.round((100 * counts[i]) / sum)}%)`} /> : null,
            )}
          </div>
          <ul className="legend-list">
            {REASONS.map((r, i) =>
              counts[i] > 0 ? (
                <li key={r.k} title={r.hint}>
                  <i style={{ background: r.color }} />
                  {r.label}
                  <b>{compact(counts[i])}</b>
                  <span className="muted">{Math.round((100 * counts[i]) / sum)}%</span>
                </li>
              ) : null,
            )}
          </ul>
        </>
      )}
    </Card>
  );
}

function ActiveNow({ s, sys }: { s: AppState; sys: DashSystem }) {
  const calls = s.calls.filter((c) => c.systemName === sys.name);
  return (
    <Card title="On the air now">
      {calls.length === 0 ? (
        <p className="empty">Quiet.</p>
      ) : (
        <ul className="air">
          {calls.map((c) => (
            <li key={c.id}>
              <button className="linkish" onClick={() => setView("radio", sys.name, "tg", c.talkgroup)}>
                <span className={`dot dot-${c.state}${c.encrypted ? " dot-enc" : ""}`} />
                <TgName r={{ tg: c.talkgroup, alphaTag: c.alphaTag }} />
              </button>
              <span className="row">
                {[...new Set(c.sources)].slice(-4).map((u) => (
                  <button key={u} className="chip" onClick={() => setView("radio", sys.name, "unit", u)}>
                    {aliasOf(s, sys.name, u) ?? u}
                  </button>
                ))}
              </span>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

type TgFilter = "all" | "new" | "unknown" | "ignored" | "encrypted";

function Talkgroups({ sys, rows, hours }: { sys: DashSystem; rows: TgRow[]; hours: number }) {
  const [filter, setFilter] = useState<TgFilter>("all");
  const [q, setQ] = useState("");
  const [sort, setSort] = useState<"secs" | "calls" | "last" | "first">("secs");
  const shown = useMemo(() => {
    const f = q.trim().toLowerCase();
    return rows
      .filter((r) => (filter === "all" ? true : filter === "new" ? r.new : filter === "unknown" ? !r.known : filter === "ignored" ? r.ignore : r.encPct > 50))
      .filter((r) => !f || String(r.tg).includes(f) || r.alphaTag.toLowerCase().includes(f))
      .sort((a, b) => (sort === "first" ? b.first - a.first : sort === "last" ? b.last - a.last : b[sort] - a[sort]));
  }, [rows, filter, q, sort]);
  const n = (k: TgFilter) => rows.filter((r) => (k === "new" ? r.new : k === "unknown" ? !r.known : k === "ignored" ? r.ignore : r.encPct > 50)).length;
  return (
    <Card
      title="Talkgroups"
      actions={
        <>
          <input className="search" placeholder="Find a talkgroup…" value={q} onChange={(e) => setQ(e.target.value)} />
        </>
      }
    >
      <div className="row">
        <Choice
          value={filter}
          onChange={setFilter}
          label="Show"
          options={[
            { v: "all", label: `All ${rows.length}` },
            { v: "new", label: `New ${n("new")}` },
            { v: "unknown", label: `Unknown ${n("unknown")}` },
            { v: "ignored", label: `Ignored ${n("ignored")}` },
            { v: "encrypted", label: `Encrypted ${n("encrypted")}` },
          ]}
        />
        <span className="spacer" />
        <Choice
          value={sort}
          onChange={setSort}
          label="Sort"
          options={[
            { v: "secs", label: "Airtime" },
            { v: "calls", label: "Calls" },
            { v: "last", label: "Latest" },
            { v: "first", label: "Newest" },
          ]}
        />
      </div>
      <div className="table-wrap tall">
        <table className="calls clickable-rows">
          <thead>
            <tr>
              <th>Talkgroup</th>
              <th>Calls</th>
              <th>Airtime</th>
              <th>By hour</th>
              <th>Last heard</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {shown.slice(0, 300).map((r) => (
              <tr key={r.tg} className={r.ignore ? "ignored" : ""} onClick={() => setView("radio", sys.name, "tg", r.tg)}>
                <td>
                  <TgName r={r} />
                  {r.alphaTag && <span className="tag mono">{r.tg}</span>}
                  {r.new && <span className="badge new">NEW</span>}
                  {!r.known && <span className="badge unknown">NOT IN FILE</span>}
                  {r.encPct > 50 && <span className="badge enc">ENC</span>}
                  {r.ignore && <span className="badge ign">IGNORED</span>}
                </td>
                <td className="mono">{r.calls}</td>
                <td className="mono">{dur(r.secs)}</td>
                <td className="spark-cell">
                  <HeatStrip v={r.hourly} titles={r.hourly.map((v, i) => `${hourLabel(hours)(i)}: ${v} calls`)} />
                </td>
                <td className="small">{ago(r.last)}</td>
                <td>
                  <IgnoreToggle sys={sys} r={r} />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {shown.length > 300 && <p className="muted small">The first 300 of {shown.length}.</p>}
    </Card>
  );
}

function Units({ s, sys, rows, total: n }: { s: AppState; sys: DashSystem; rows: UnitRow[]; total: number }) {
  const [q, setQ] = useState("");
  const f = q.trim().toLowerCase();
  const shown = rows.filter((r) => !f || String(r.unit).includes(f) || (aliasOf(s, sys.name, r.unit) ?? r.alias).toLowerCase().includes(f));
  return (
    <Card title={`Radios (${compact(n)})`} actions={<input className="search" placeholder="Find a radio…" value={q} onChange={(e) => setQ(e.target.value)} />}>
      <div className="table-wrap tall">
        <table className="calls clickable-rows">
          <thead>
            <tr>
              <th>Radio</th>
              <th title="The talkgroup it last affiliated with">Affiliated</th>
              <th>Transmitted</th>
              <th>Talks on</th>
              <th>Last heard</th>
            </tr>
          </thead>
          <tbody>
            {shown.slice(0, 300).map((r) => {
              const alias = aliasOf(s, sys.name, r.unit) ?? r.alias;
              return (
                <tr key={r.unit} onClick={() => setView("radio", sys.name, "unit", r.unit)}>
                  <td>
                    {alias ? <b>{alias}</b> : null} <span className="mono muted">{r.unit}</span>
                    {r.new && <span className="badge new">NEW</span>}
                  </td>
                  <td className="mono">{r.aff ?? "—"}</td>
                  <td className="mono">
                    {r.tx} · {dur(r.txSecs)}
                  </td>
                  <td className="small">{r.tgs.slice(0, 3).map(([tg, k]) => `${tg}×${k}`).join(" · ") || "—"}</td>
                  <td className="small">{ago(r.lastTx ?? r.last)}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </Card>
  );
}

/** Who talks on what: the busiest talkgroups and the radios on them. */
function Network({ s, sys, tgs, units }: { s: AppState; sys: DashSystem; tgs: TgRow[]; units: UnitRow[] }) {
  const top = units.filter((u) => u.tgs.length).sort((a, b) => b.tx - a.tx).slice(0, 18);
  const tgSet = new Map<number, number>();
  for (const u of top) for (const [tg, n] of u.tgs) tgSet.set(tg, (tgSet.get(tg) ?? 0) + n);
  const tgTop = [...tgSet].sort((a, b) => b[1] - a[1]).slice(0, 14);
  const name = (tg: number) => tgs.find((r) => r.tg === tg)?.alphaTag || String(tg);
  const left: Node[] = tgTop.map(([tg, n]) => ({ id: `t${tg}`, label: name(tg), weight: n }));
  const right: Node[] = top.map((u) => ({ id: `u${u.unit}`, label: aliasOf(s, sys.name, u.unit) || u.alias || String(u.unit), weight: u.tx }));
  const edges = top.flatMap((u) => u.tgs.filter(([tg]) => tgSet.has(tg) && tgTop.some((t) => t[0] === tg)).map(([tg, n]) => ({ a: `t${tg}`, b: `u${u.unit}`, w: n })));
  if (left.length < 2 || right.length < 2) return null;
  return (
    <Card title="Who talks on what">
      <Bipartite
        left={left}
        right={right}
        edges={edges}
        leftTitle="Talkgroups"
        rightTitle="Radios (busiest)"
        onClick={(n) => (n.id.startsWith("t") ? setView("radio", sys.name, "tg", n.id.slice(1)) : setView("radio", sys.name, "unit", n.id.slice(1)))}
      />
      <Hint>Hover a talkgroup to see its regulars, or a radio to see where it talks. Thick links are frequent. Click either to open it.</Hint>
    </Card>
  );
}

function Lengths({ h }: { h: LengthHistogram | undefined }) {
  if (!h) return null;
  const labels = h.edges.map((e, i) => (i === h.edges.length - 1 ? `${e}+ s` : `${e}–${h.edges[i + 1]}`));
  return (
    <Card title="Call lengths">
      <Histogram counts={h.counts} labels={labels} title="calls by length, seconds" />
      <Hint>Seconds. Dispatch traffic clusters at 2–8 s; long calls are often patches, open mics or data.</Hint>
    </Card>
  );
}

function SystemView({ s, sys }: { s: AppState; sys: DashSystem }) {
  const [hours, setHours] = useState<Hours>("24");
  const h = Number(hours);
  const live = running(s);
  const every = live ? 30_000 : 0;
  const tgsR = usePolled(`tgs:${sys.name}:${hours}`, () => radioQuery({ what: "talkgroups", system: sys.name, hours: h, limit: 2000 }), every);
  const unitsR = usePolled(`units:${sys.name}:${hours}`, () => radioQuery({ what: "units", system: sys.name, hours: h, limit: 1000 }), every);
  const sumR = usePolled(`sum:${sys.name}`, () => radioQuery({ what: "summary" }), every * 2);
  const lenR = usePolled(`len:${sys.name}:${hours}`, () => radioQuery({ what: "lengths", system: sys.name, hours: h }), every * 2);
  const tgs = (tgsR?.rows ?? []) as TgRow[];
  const units = (unitsR?.rows ?? []) as UnitRow[];
  const summary = sumR?.systems?.[sys.name];
  const calls = tgs.reduce((a, r) => a + r.calls, 0);
  const secs = tgs.reduce((a, r) => a + r.secs, 0);
  const busy = [...tgs].sort((a, b) => b.calls - a.calls).slice(0, 40);
  const error = tgsR?.error;
  return (
    <div className="stack">
      <div className="row">
        <span className="spacer" />
        <Choice value={hours} options={HOURS} onChange={setHours} label="Span" />
      </div>
      {error && <p className="empty">{error}</p>}
      <div className="kpis">
        <Stat size="hero" label="Calls" value={compact(calls)} sub={`in the last ${h === 24 ? "24 hours" : "7 days"}`} />
        <Stat label="Airtime" value={hmm(secs)} sub={calls ? `${dur(secs / calls)} a call on average` : undefined} />
        <Stat label="Talkgroups heard" value={compact(tgsR?.total ?? tgs.length)} sub={summary ? `${summary.talkgroups} known to this registry` : undefined} />
        <Stat label="Radios heard" value={compact(unitsR?.total ?? units.length)} sub={summary ? `${summary.units1h} in the last hour` : undefined} />
        <Stat
          label="New talkgroups"
          value={summary ? summary.newTgs.length : "—"}
          level={summary && summary.newTgs.length ? "warn" : undefined}
          sub={summary?.baseline === false ? "learning what's usual (first hour)" : "first heard in the last day"}
          hint="Talkgroups this recorder had never heard before today. New ones can be a re-band, an event, or a misconfigured radio."
        />
        <Stat label="Unknown" value={summary ? summary.unknownTgs24h : "—"} sub="heard today, not in the talkgroup file" hint="Add them to the file to name them (and to record them, if unknown talkgroups are off)." />
      </div>
      <div className="columns">
        <Reasons name={sys.name} range={hours === "24" ? "24h" : "7d"} />
        <ActiveNow s={s} sys={sys} />
      </div>
      {busy.length > 0 && (
        <Card title="Activity by hour">
          <Heatmap
            rows={busy.map((r) => ({ label: r.alphaTag || String(r.tg), sub: `${r.calls} calls`, v: r.hourly }))}
            colLabel={hourLabel(h)}
            valueText={(v) => `${v} call${v === 1 ? "" : "s"}`}
            onRow={(i) => setView("radio", sys.name, "tg", busy[i].tg)}
          />
          <Hint>The busiest {busy.length} talkgroups, an hour a column (newest on the right). A row that lights up at the same hours each day is a routine; a sudden block is an incident.</Hint>
        </Card>
      )}
      <Network s={s} sys={sys} tgs={tgs} units={units} />
      <Talkgroups sys={sys} rows={tgs} hours={h} />
      <div className="columns">
        <Units s={s} sys={sys} rows={units} total={unitsR?.total ?? units.length} />
        <Lengths h={lenR?.histogram} />
      </div>
    </div>
  );
}

// ── one talkgroup ────────────────────────────────────────────────────────────

function TgView({ s, sys, tg }: { s: AppState; sys: DashSystem; tg: number }) {
  const r = usePolled(`tg:${sys.name}:${tg}`, () => radioQuery({ what: "tg", system: sys.name, key: tg, hours: 168 }), running(s) ? 30_000 : 0);
  const row = r?.row as TgRow | null | undefined;
  if (!r) return <p className="empty">Loading…</p>;
  if (!row) return <p className="empty">Talkgroup {tg} hasn't been heard on {sys.name}.</p>;
  // The week's hours laid out by local day (a row each, today at the bottom) and local hour.
  const week = r.week ?? [];
  const nowH = Math.floor(Date.now() / 3600_000);
  const byDay = new Map<string, { label: string; v: number[] }>();
  week.forEach((v, j) => {
    const d = new Date((nowH - (week.length - 1) + j) * 3600_000);
    const k = d.toDateString();
    if (!byDay.has(k)) byDay.set(k, { label: d.toLocaleDateString([], { weekday: "short", day: "numeric" }), v: Array(24).fill(0) });
    byDay.get(k)!.v[d.getHours()] += v;
  });
  const days = [...byDay.values()];
  const talkers = r.talkers ?? [];
  const maxN = Math.max(1, ...talkers.map((t) => t.n));
  return (
    <div className="stack">
      <div className="row">
        <button className="btn ghost small" onClick={() => setView("radio", sys.name)}>
          ← {sys.name}
        </button>
      </div>
      <section className="hero">
        <div>
          <h2 className="big">{row.alphaTag || `Talkgroup ${tg}`}</h2>
          <p className="muted">
            TG {tg} · {sys.name} · first heard {clockAt(row.first)} · last {ago(row.last)}
            {!row.known && " · not in the talkgroup file"}
          </p>
        </div>
        <div className="row">
          {row.ignore && <span className="badge ign">IGNORED</span>}
          <IgnoreToggle sys={sys} r={row} />
        </div>
      </section>
      <div className="kpis">
        <Stat label="Calls, 7 days" value={compact(row.calls)} sub={`${row.totalCalls} since first heard`} />
        <Stat label="Airtime, 7 days" value={hmm(row.secs)} />
        <Stat label="Encrypted" value={`${row.encPct}%`} sub="of its calls" />
        <Stat label="Affiliated now" value={(r.affiliated ?? []).length} sub="radios" />
      </div>
      <Card title="Its week">
        <Heatmap rows={days} colLabel={(i) => `${String(i).padStart(2, "0")}:00–${String(i + 1).padStart(2, "0")}:00`} valueText={(v) => `${v} calls`} rowH={22} />
        <Hint>A row a day, an hour a column, midnight on the left. Routines show as columns that light up every day.</Hint>
      </Card>
      <div className="columns">
        <Card title="Who talks here">
          {talkers.length === 0 ? (
            <p className="empty">No radio IDs heard on it.</p>
          ) : (
            <ul className="barlist">
              {talkers.map((t) => (
                <li key={t.unit}>
                  <button className="linkish" onClick={() => setView("radio", sys.name, "unit", t.unit)}>
                    {aliasOf(s, sys.name, t.unit) ?? (t.alias || t.unit)}
                  </button>
                  <span className="bar" style={{ width: `${(t.n / maxN) * 100}%` }} />
                  <b className="mono">{t.n}</b>
                </li>
              ))}
            </ul>
          )}
        </Card>
        <div className="stack">
          <Lengths h={r.lengths} />
          <Card title="Affiliated with it">
            {(r.affiliated ?? []).length === 0 ? (
              <p className="empty">None right now.</p>
            ) : (
              <div className="row">
                {(r.affiliated ?? []).map((a) => (
                  <button key={a.unit} className="chip" onClick={() => setView("radio", sys.name, "unit", a.unit)}>
                    {aliasOf(s, sys.name, a.unit) ?? (a.alias || a.unit)}
                  </button>
                ))}
              </div>
            )}
          </Card>
        </div>
      </div>
    </div>
  );
}

// ── one radio ────────────────────────────────────────────────────────────────

function UnitView({ s, sys, unit }: { s: AppState; sys: DashSystem; unit: number }) {
  const r = usePolled(`unit:${sys.name}:${unit}`, () => radioQuery({ what: "unit", system: sys.name, key: unit }), running(s) ? 15_000 : 0) as RadioResult | null;
  const row = r?.row as UnitRow | null | undefined;
  if (!r) return <p className="empty">Loading…</p>;
  if (!row) return <p className="empty">Radio {unit} hasn't been heard on {sys.name}.</p>;
  const alias = aliasOf(s, sys.name, unit) ?? row.alias;
  const tgs = r.tgs ?? [];
  const partners = r.partners ?? [];
  const affTag = row.aff !== null ? (tgs.find((t) => t.tg === row.aff)?.alphaTag ?? "") : "";
  return (
    <div className="stack">
      <div className="row">
        <button className="btn ghost small" onClick={() => setView("radio", sys.name)}>
          ← {sys.name}
        </button>
      </div>
      <section className="hero">
        <div>
          <h2 className="big">{alias || `Radio ${unit}`}</h2>
          <p className="muted">
            Unit {unit} · {sys.name} · first heard {clockAt(row.first)} · last {ago(row.last)}
            {row.reg === false && " · deregistered"}
          </p>
        </div>
        {row.new && <span className="badge new">NEW</span>}
      </section>
      <div className="kpis">
        <Stat label="Transmissions" value={compact(row.tx)} sub={dur(row.txSecs) + " on the air"} />
        <Stat label="Last keyed up" value={row.lastTx ? ago(row.lastTx) : "never"} />
        <Stat
          label="Affiliated with"
          value={row.aff !== null ? affTag || row.aff : "—"}
          sub={row.affT ? `since ${clockAt(row.affT)}` : "no affiliation heard"}
          onClick={row.aff !== null ? () => setView("radio", sys.name, "tg", row.aff!) : undefined}
        />
        <Stat label="Talks with" value={partners.length} sub="radios heard on its calls" />
      </div>
      <div className="columns">
        <Card title="Its circle">
          {tgs.length + partners.length === 0 ? (
            <p className="empty">Not heard transmitting yet.</p>
          ) : (
            <RadialEgo
              center={{ label: alias ? alias.slice(0, 12) : String(unit), sub: alias ? String(unit) : undefined }}
              inner={tgs.map((t) => ({ id: `t${t.tg}`, label: t.alphaTag || String(t.tg), weight: t.n }))}
              outer={partners.map((p) => ({ id: `u${p.unit}`, label: aliasOf(s, sys.name, p.unit) || p.alias || String(p.unit), weight: p.n }))}
              onClick={(n) => (n.id.startsWith("t") ? setView("radio", sys.name, "tg", n.id.slice(1)) : setView("radio", sys.name, "unit", n.id.slice(1)))}
            />
          )}
          <Hint>Inner ring: the talkgroups it transmits on. Outer ring: radios heard on the same calls (or called directly). Bigger is more often.</Hint>
        </Card>
        <div className="stack">
          <Card title="Latest transmissions">
            <ul className="timeline">
              {(r.recent ?? []).map((t, i) => (
                <li key={i}>
                  <span className="muted small">{clockAt(t.t, true)}</span>
                  <button className="linkish" onClick={() => setView("radio", sys.name, "tg", t.tg)}>
                    {t.alphaTag || t.tg}
                  </button>
                  <span className="mono small">{num(t.secs, 1)} s</span>
                </li>
              ))}
            </ul>
          </Card>
          <Card title="Affiliations">
            {(r.affiliations ?? []).length === 0 ? (
              <p className="empty">None heard.</p>
            ) : (
              <ul className="timeline">
                {(r.affiliations ?? []).map((a, i) => (
                  <li key={i}>
                    <span className="muted small">{clockAt(a.t, true)}</span>
                    <button className="linkish" onClick={() => setView("radio", sys.name, "tg", a.tg)}>
                      {a.alphaTag || a.tg}
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </Card>
        </div>
      </div>
    </div>
  );
}

// ── the page ─────────────────────────────────────────────────────────────────

export function RadioPage() {
  const s = useApp();
  const systems = dashSystems(s);
  const name = s.path[0] ?? systems[0]?.name;
  const sys = systems.find((x) => x.name === name);
  if (!systems.length) return <p className="empty">No systems set up yet.</p>;
  if (!sys) return <p className="empty">No system called {name}.</p>;
  const [, what, key] = s.path;
  return (
    <div className="page">
      {systems.length > 1 && (
        <div className="filter-row" role="tablist" aria-label="System">
          {systems.map((x) => (
            <button key={x.name} className={`btn ghost small${x.name === sys.name ? " on" : ""}`} onClick={() => setView("radio", x.name)}>
              <span className="sys-dot" style={{ background: systemColor(s.config, x.name) }} />
              {x.name}
            </button>
          ))}
        </div>
      )}
      {!running(s) && <p className="muted small"><Light level="idle" /> Not recording — this is what was heard before.</p>}
      {what === "tg" && key ? <TgView s={s} sys={sys} tg={Number(key)} /> : what === "unit" && key ? <UnitView s={s} sys={sys} unit={Number(key)} /> : <SystemView s={s} sys={sys} />}
    </div>
  );
}
