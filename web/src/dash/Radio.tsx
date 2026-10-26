// The radio system as heard: which talkgroups are busy (and which are new,
// unknown, or ignored), which radios are on the air, what they're
// affiliated with and who they talk with — and, for one talkgroup or one
// radio, its week and its connections.

import { useEffect, useMemo, useRef, useState } from "react";
import { formatMhz, systemColor } from "../config.ts";
import { radioQuery, setTalkgroupFields, setTalkgroupIgnore, setUnitName, setView, usePolled, useApp, type AppState } from "../controller.ts";
import { BackBar, Bipartite, Card, Choice, Heatmap, HeatStrip, Histogram, Light, RadialEgo, Stat, SystemTag, type Node } from "../charts.tsx";
import { ago, clockAt, compact, dur, hmm, num } from "../fmt.ts";
import { Field } from "../inputs.tsx";
import { showTodo } from "../Setup.tsx";
import { parseTalkgroupCsv, type Talkgroup } from "../talkgroups.ts";
import type { CipherSeen, LengthHistogram, OutOfBandRow, RadioResult, TgRow, UnitRow } from "../protocol.ts";
import { ActiveCalls, aliasOf, Log } from "./Calls.tsx";
import { Tones } from "./Tones.tsx";
import { coverage, dashSystems, goTo, K, KIND_LABEL, OUT_OF_BAND, running, systemRadioHealth, total, useHistory, type DashSystem, type Health } from "./data.ts";

type Hours = "24" | "168";
const HOURS: { v: Hours; label: string }[] = [
  { v: "24", label: "24 h" },
  { v: "168", label: "7 days" },
];

/** Why calls weren't recorded (and were), in the order shown; `at`: the part of the system's page listing them. */
const REASONS: { k: string; label: string; color: string; hint: string; at?: string }[] = [
  { k: "recorded", label: "Recorded", color: "var(--series-1)", hint: "" },
  { k: "monitored", label: "Followed only", color: "var(--series-3)", hint: "Followed, not recorded" },
  { k: "ignored", label: "Ignored", color: "var(--muted-mark)", hint: "Set to Ignore in the talkgroup file" },
  { k: "encrypted", label: "Encrypted", color: "var(--series-6)", hint: "Encrypted calls are off" },
  { k: "unknown_tg", label: "Not in the file", color: "var(--series-4)", hint: "Unknown talkgroups are off" },
  { k: "no_recorder", label: "No recorder", color: "var(--critical)", hint: "All recorders busy" },
  { k: "no_source", label: "Out of band", color: "var(--serious)", hint: "No source covers the frequency: which ones", at: OUT_OF_BAND },
];

const hourLabel = (hours: number) => (i: number) => {
  const t = (Math.floor(Date.now() / 3600_000) - (hours - 1 - i)) * 3600;
  return clockAt(t);
};

/**
 * `sys`'s talkgroup file, parsed — `null` only in the brief window before
 * it's loaded (just after connecting). Kept in sync with every local edit
 * the moment it's made, unlike the polled talkgroup rows below, which can
 * lag 30 s (while recording) or indefinitely (while idle/replaying).
 */
function useLiveTalkgroups(s: AppState, sys: DashSystem): Map<number, Talkgroup> | null {
  const csv = s.talkgroups[sys.name];
  return useMemo(() => (csv === undefined ? null : parseTalkgroupCsv(csv)), [csv]);
}

/** `row` with `known` / `ignore` / `alphaTag` taken from `live` (the talkgroup
 * file as it is right now) in place of the polled snapshot's, so a toggle or
 * an edit shows correctly at once instead of waiting on the next poll. */
function withLiveTag(row: TgRow, live: Map<number, Talkgroup> | null): TgRow {
  if (!live) return row;
  const e = live.get(row.tg);
  return { ...row, known: live.has(row.tg), ignore: e?.ignore ?? false, alphaTag: e?.alphaTag || row.alphaTag };
}
const withLiveTags = (rows: TgRow[], live: Map<number, Talkgroup> | null) => (live ? rows.map((r) => withLiveTag(r, live)) : rows);

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
      title={r.ignore ? "Record again" : "Stop recording"}
    >
      {r.ignore ? "Ignored ✕" : "Ignore"}
    </button>
  );
}

// ── the system ───────────────────────────────────────────────────────────────

function Reasons({ name, range, health }: { name: string; range: "24h" | "7d"; health: Health }) {
  const hist = useHistory(REASONS.map((r) => K.sys(name, `why/${r.k}`)), range, 48);
  const counts = REASONS.map((r) => Math.round(total(hist?.[K.sys(name, `why/${r.k}`)]) ?? 0));
  const sum = counts.reduce((a, b) => a + b, 0);
  return (
    <Card title="What happened to calls" level={health.level} why={health.why} onWhy={goTo(health)}>
      {sum === 0 ? (
        <p className="empty">No calls yet.</p>
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
                <li key={r.k} title={r.hint} className={r.at ? "opens" : undefined} onClick={r.at ? () => setView("radio", name, r.at!) : undefined}>
                  <i style={{ background: r.color }} />
                  {r.at ? <button className="linkish">{r.label} →</button> : r.label}
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
                  {r.encPct > 50 && (
                    <span className="badge enc" title={r.ciphers.map(cipherText).join(", ")}>
                      ENC
                    </span>
                  )}
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
      {shown.length > 300 && <p className="muted small">300 of {shown.length} shown.</p>}
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
              <th>Affiliated</th>
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
    </Card>
  );
}

function Lengths({ h }: { h: LengthHistogram | undefined }) {
  if (!h) return null;
  const labels = h.edges.map((e, i) => (i === h.edges.length - 1 ? `${e}+ s` : `${e}–${h.edges[i + 1]}`));
  return (
    <Card title="Call lengths">
      <Histogram counts={h.counts} labels={labels} title="calls by length, seconds" />
    </Card>
  );
}

/**
 * The frequencies granted that no source covered, in the span: how often,
 * when last, and the nearest source with how far past its usable edge each
 * is. Shown when there are some, or when asked for (`focus`: scrolled to).
 */
function OutOfBand({ s, sys, rows, hours, focus }: { s: AppState; sys: DashSystem; rows: OutOfBandRow[]; hours: number; focus: boolean }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (focus) ref.current?.scrollIntoView({ block: "start" });
  }, [focus]);
  if (!rows.length && !focus) return null;
  const n = rows.reduce((a, r) => a + r.calls, 0);
  return (
    <div ref={ref}>
      <Card title={`Out of band (${rows.length})`} actions={rows.length > 0 && <span className="muted small">{compact(n)} calls missed</span>}>
        {rows.length === 0 ? (
          <p className="empty">None in the last {hours === 24 ? "24 hours" : "7 days"}: every frequency granted was in a source's band.</p>
        ) : (
          <>
            <p className="muted small">
              Calls granted on these frequencies weren't recorded: no source covers them. Move a source's centre toward them, widen its sample rate, or add a source.
            </p>
            <div className="table-wrap">
              <table className="calls">
                <thead>
                  <tr>
                    <th>MHz</th>
                    <th>Calls</th>
                    <th>By hour</th>
                    <th>Last</th>
                    <th>Nearest source</th>
                  </tr>
                </thead>
                <tbody>
                  {rows.map((r) => {
                    const c = coverage(s, r.freqHz);
                    return (
                      <tr key={r.freqHz}>
                        <td className="mono">{formatMhz(r.freqHz)}</td>
                        <td className="mono" title={`${r.totalCalls} all told`}>{r.calls}</td>
                        <td className="spark-cell">
                          <HeatStrip v={r.hourly} titles={r.hourly.map((v, i) => `${hourLabel(hours)(i)}: ${v} calls`)} />
                        </td>
                        <td className="small">{ago(r.last)}</td>
                        <td className="small">{!c ? "—" : c.beyondHz > 0 ? `${c.src.label}: ${c.beyondHz >= 1e6 ? `${num(c.beyondHz / 1e6, 2)} MHz` : `${num(c.beyondHz / 1000, 1)} kHz`} past its usable edge` : `${c.src.label} (covers it now)`}</td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </>
        )}
      </Card>
    </div>
  );
}

function SystemView({ s, sys, focus }: { s: AppState; sys: DashSystem; focus?: string }) {
  const [hours, setHours] = useState<Hours>("24");
  const h = Number(hours);
  const live = running(s);
  const every = live ? 30_000 : 0;
  const tgsR = usePolled(`tgs:${sys.name}:${hours}`, () => radioQuery({ what: "talkgroups", system: sys.name, hours: h, limit: 2000 }), every);
  const unitsR = usePolled(`units:${sys.name}:${hours}`, () => radioQuery({ what: "units", system: sys.name, hours: h, limit: 1000 }), every);
  const sumR = usePolled(`sum:${sys.name}`, () => radioQuery({ what: "summary" }), every * 2);
  const lenR = usePolled(`len:${sys.name}:${hours}`, () => radioQuery({ what: "lengths", system: sys.name, hours: h }), every * 2);
  const freqsR = usePolled(`oob:${sys.name}:${hours}`, () => radioQuery({ what: "freqs", system: sys.name, hours: h }), every * 2);
  const liveTgs = useLiveTalkgroups(s, sys);
  const tgs = withLiveTags((tgsR?.rows ?? []) as TgRow[], liveTgs);
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
        <Stat size="hero" label="Calls" value={compact(calls)} sub={`last ${h === 24 ? "24 hours" : "7 days"}`} />
        <Stat label="Airtime" value={hmm(secs)} sub={calls ? `${dur(secs / calls)} avg call` : undefined} />
        <Stat label="Talkgroups heard" value={compact(tgsR?.total ?? tgs.length)} sub={summary ? `${summary.talkgroups} known` : undefined} />
        <Stat label="Radios heard" value={compact(unitsR?.total ?? units.length)} sub={summary ? `${summary.units1h} last hour` : undefined} />
        <Stat
          label="New talkgroups"
          value={summary ? summary.newTgs.length : "—"}
          level={summary && summary.newTgs.length ? "warn" : undefined}
          sub={summary?.baseline === false ? "learning (first hour)" : "first heard in 24 h"}
        />
        <Stat label="Unknown" value={summary ? summary.unknownTgs24h : "—"} sub="not in the talkgroup file" />
      </div>
      <ActiveCalls s={s} system={sys.name} />
      <div className="columns">
        <Reasons name={sys.name} range={hours === "24" ? "24h" : "7d"} health={systemRadioHealth(s, sys)} />
        <Lengths h={lenR?.histogram} />
      </div>
      <OutOfBand s={s} sys={sys} rows={freqsR?.outOfBand ?? []} hours={h} focus={focus === OUT_OF_BAND} />
      {busy.length > 0 && (
        <Card title={`Busiest ${busy.length} by hour`}>
          <Heatmap
            rows={busy.map((r) => ({ label: r.alphaTag || String(r.tg), sub: `${r.calls} calls`, v: r.hourly }))}
            colLabel={hourLabel(h)}
            valueText={(v) => `${v} call${v === 1 ? "" : "s"}`}
            onRow={(i) => setView("radio", sys.name, "tg", busy[i].tg)}
          />
        </Card>
      )}
      <Network s={s} sys={sys} tgs={tgs} units={units} />
      <Talkgroups sys={sys} rows={tgs} hours={h} />
      <Units s={s} sys={sys} rows={units} total={unitsR?.total ?? units.length} />
      <Log s={s} system={sys.name} />
      <Tones s={s} sys={sys} hours={h} />
    </div>
  );
}

// ── one talkgroup ────────────────────────────────────────────────────────────

/** Alpha Tag / Description / Tag / Category, editable in place — only ever
 * from the talkgroup file itself: `TgRow` carries none of these beyond the
 * alpha tag, so there's nothing to fall back to while `live` hasn't loaded. */
function TgFields({ sys, tg, live }: { sys: DashSystem; tg: number; live: Map<number, Talkgroup> | null }) {
  const t = live?.get(tg);
  const loading = live === null;
  return (
    <Card title="Talkgroup file">
      <div className="row">
        <TgTextField sys={sys} tg={tg} field="alphaTag" label="Alpha Tag" value={t?.alphaTag ?? ""} disabled={loading} />
        <TgTextField sys={sys} tg={tg} field="description" label="Description" value={t?.description ?? ""} disabled={loading} />
        <TgTextField sys={sys} tg={tg} field="tag" label="Tag" value={t?.tag ?? ""} disabled={loading} />
        <TgTextField sys={sys} tg={tg} field="group" label="Category" value={t?.group ?? ""} disabled={loading} />
      </div>
    </Card>
  );
}

function TgTextField({
  sys,
  tg,
  field,
  label,
  value,
  disabled,
}: {
  sys: DashSystem;
  tg: number;
  field: "alphaTag" | "description" | "tag" | "group";
  label: string;
  value: string;
  disabled?: boolean;
}) {
  const [text, setText] = useState(value);
  useEffect(() => setText(value), [value]);
  const save = () => {
    if (text !== value) setTalkgroupFields(sys.name, tg, { [field]: text } as Partial<Pick<Talkgroup, "alphaTag" | "description" | "tag" | "group">>);
  };
  return (
    <Field label={label}>
      <input value={text} disabled={disabled} onChange={(e) => setText(e.target.value)} onBlur={save} onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()} />
    </Field>
  );
}

function TgView({ s, sys, tg }: { s: AppState; sys: DashSystem; tg: number }) {
  const r = usePolled(`tg:${sys.name}:${tg}`, () => radioQuery({ what: "tg", system: sys.name, key: tg, hours: 168 }), running(s) ? 30_000 : 0);
  const live = useLiveTalkgroups(s, sys);
  const rawRow = r?.row as TgRow | null | undefined;
  const row = rawRow ? withLiveTag(rawRow, live) : rawRow;
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
      <TgFields sys={sys} tg={tg} live={live} />
      <div className="kpis">
        <Stat label="Calls, 7 days" value={compact(row.calls)} sub={`${row.totalCalls} since first heard`} />
        <Stat label="Airtime, 7 days" value={hmm(row.secs)} />
        <Stat label="Encrypted" value={`${row.encPct}%`} />
        <Stat label="Affiliated now" value={(r.affiliated ?? []).length} sub="radios" />
      </div>
      <Card title="Last 7 days">
        <Heatmap rows={days} colLabel={(i) => `${String(i).padStart(2, "0")}:00–${String(i + 1).padStart(2, "0")}:00`} valueText={(v) => `${v} calls`} rowH={22} />
      </Card>
      <div className="columns">
        <Card title="Who talks here">
          {talkers.length === 0 ? (
            <p className="empty">No radio IDs heard on it.</p>
          ) : (
            <ul className="barlist">
              {talkers.map((t) => (
                <li key={t.unit} className="opens" onClick={() => setView("radio", sys.name, "unit", t.unit)}>
                  <button className="linkish">
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
          {row.ciphers.length > 0 && <Ciphers ciphers={row.ciphers} />}
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

const hex = (v: number, digits: number) => `0x${v.toString(16).toUpperCase().padStart(digits, "0")}`;
const cipherText = (c: CipherSeen) => `${c.name} key ${hex(c.keyId, 4)}`;

/** The ciphers its calls were encrypted with: what a user with keys needs. */
function Ciphers({ ciphers }: { ciphers: CipherSeen[] }) {
  return (
    <Card title="Encryption heard">
      <div className="table-wrap">
        <table className="calls">
          <thead>
            <tr>
              <th>Cipher</th>
              <th>Key ID</th>
              <th>Calls</th>
              <th>Last heard</th>
            </tr>
          </thead>
          <tbody>
            {ciphers.map((c) => (
              <tr key={`${c.algid}:${c.keyId}`}>
                <td>
                  {c.name} <span className="tag mono">{hex(c.algid, 2)}</span>
                </td>
                <td className="mono">{hex(c.keyId, 4)}</td>
                <td className="mono">{c.calls}</td>
                <td className="small">{ago(c.last)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </Card>
  );
}

// ── one radio ────────────────────────────────────────────────────────────────

/**
 * The radio's name, editable in place — adds or updates a literal row in
 * the system's unit names file (ahead of every pattern, so it always wins).
 * Saving always writes the name; if the system's current "Unit names" mode
 * would hide a name set here (it prefers aliases heard over the air, or
 * shows none), a note says so and links to the Setup field that controls it.
 */
function UnitNameField({ s, sys, unit, alias }: { s: AppState; sys: DashSystem; unit: number; alias: string | undefined }) {
  const [text, setText] = useState(alias ?? "");
  useEffect(() => setText(alias ?? ""), [alias]);
  const conventional = sys.kind === "conventional";
  const names = s.config?.systems.find((x) => x.shortName === sys.name)?.unitNames ?? s.config?.conventional.find((x) => x.shortName === sys.name)?.unitNames;
  const mode = names?.mode || "user";
  const hidden = mode === "ota" || mode === "none";
  const save = () => {
    if (text !== (alias ?? "")) setUnitName(sys.name, conventional, unit, text);
  };
  return (
    <Card title="Name">
      <Field label="Name">
        <input value={text} onChange={(e) => setText(e.target.value)} onBlur={save} onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()} />
      </Field>
      {hidden && (
        <p className="muted small">
          This system's "Unit names" mode ({mode === "ota" ? "aliases heard first" : "no names"}) means a name set here won't show until that's changed.{" "}
          <button className="btn ghost small" onClick={() => showTodo(conventional ? `convunits-${sys.name}` : `units-${sys.name}`)}>
            Open in Setup
          </button>
        </p>
      )}
    </Card>
  );
}

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
      <UnitNameField s={s} sys={sys} unit={unit} alias={alias} />
      <div className="kpis">
        <Stat label="Transmissions" value={compact(row.tx)} sub={dur(row.txSecs) + " on the air"} />
        <Stat label="Last keyed up" value={row.lastTx ? ago(row.lastTx) : "never"} />
        <Stat
          label="Affiliated with"
          value={row.aff !== null ? affTag || row.aff : "—"}
          sub={row.affT ? `since ${clockAt(row.affT)}` : "none heard"}
          onClick={row.aff !== null ? () => setView("radio", sys.name, "tg", row.aff!) : undefined}
        />
        <Stat label="Talks with" value={partners.length} sub="radios" />
      </div>
      <div className="columns">
        <Card title="Talkgroups and radios">
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

/** Which system a page is about: its colour, name, kind and site. */
function SystemHeader({ s, sys }: { s: AppState; sys: DashSystem }) {
  // Its sites as their control channels say: "SysID 1A2 · site 1-3", or "SysID 1A2 · sites 1-3, 1-4".
  const ids = sys.sites.flatMap((x) => (x.status ? [x.status.identity] : []));
  const sysId = ids.find((i) => i.sysId != null)?.sysId;
  const numbers = ids.filter((i) => i.site != null).map((i) => `${i.rfss ?? "?"}-${i.site}`);
  const site = [sysId != null ? `SysID ${sysId.toString(16).toUpperCase()}` : "", numbers.length ? `site${numbers.length > 1 ? "s" : ""} ${numbers.join(", ")}` : ""].filter(Boolean).join(" · ");
  return (
    <div className="sys-header" style={{ borderLeftColor: systemColor(s.config, sys.name) }}>
      <div>
        <h2 className="big">{sys.name}</h2>
        <p className="muted small">
          {KIND_LABEL[sys.kind]}
          {site ? ` · ${site}` : ""}
        </p>
      </div>
    </div>
  );
}

/** Several systems: each one separately, side by side — never added together. */
function AllSystems({ s, systems }: { s: AppState; systems: DashSystem[] }) {
  const live = running(s);
  const names = systems.map((x) => x.name).join(",");
  const sum = usePolled(`sum-all:${names}`, () => radioQuery({ what: "summary" }), live ? 60_000 : 0);
  const tops = usePolled(`tops:${names}`, () => Promise.all(systems.map((x) => radioQuery({ what: "talkgroups", system: x.name, hours: 24, limit: 5 }))), live ? 60_000 : 0);
  return (
    <div className="card-grid wide">
      {systems.map((x, i) => {
        const sm = sum?.systems?.[x.name];
        const top = (tops?.[i]?.rows ?? []) as TgRow[];
        const onAir = s.calls.filter((c) => c.systemName === x.name);
        const h = systemRadioHealth(s, x);
        return (
          <Card
            key={x.name}
            className="sys-section"
            level={h.level}
            why={h.why}
            onWhy={goTo(h)}
            title={<SystemTag config={s.config} name={x.name} />}
            onTitle={() => setView("radio", x.name)}
            actions={<span className="badge-kind">{KIND_LABEL[x.kind]}</span>}
          >
            <div className="mini-stats">
              <div>
                <b>{sm ? compact(sm.tgs24h) : "—"}</b>
                <span>talkgroups, 24 h</span>
              </div>
              <div>
                <b>{sm ? compact(sm.units24h) : "—"}</b>
                <span>radios, 24 h</span>
              </div>
              <div>
                <b className={sm?.newTgs.length ? "accent" : ""}>{sm ? sm.newTgs.length : "—"}</b>
                <span>new talkgroups</span>
              </div>
              <div>
                <b>{sm ? sm.unknownTgs24h : "—"}</b>
                <span>not in its file</span>
              </div>
            </div>
            {top.length > 0 ? (
              <ul className="barlist">
                {top.map((r) => (
                  <li key={r.tg} className="opens" onClick={() => setView("radio", x.name, "tg", r.tg)}>
                    <button className="linkish">
                      <TgName r={r} />
                    </button>
                    <span className="bar" style={{ width: `${(r.secs / Math.max(1, top[0].secs)) * 100}%`, background: systemColor(s.config, x.name) }} />
                    <b className="mono">{r.calls}</b>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="empty">Nothing heard in the last day.</p>
            )}
            {onAir.length > 0 && (
              <div className="row tg-chips">
                {[...new Map(onAir.map((c) => [c.talkgroup, c])).values()].slice(0, 6).map((c) => (
                  <button key={c.talkgroup} className="chip" onClick={() => setView("radio", x.name, "tg", c.talkgroup)}>
                    <span className={`dot dot-${c.state}`} />
                    {c.alphaTag || c.talkgroup}
                  </button>
                ))}
              </div>
            )}
            {sm && sm.newTgs.length > 0 && (
              <p className="small muted">
                New: {sm.newTgs.slice(0, 6).map((t) => t.alphaTag || t.tg).join(", ")}
                {sm.newTgs.length > 6 ? "…" : ""}
              </p>
            )}
          </Card>
        );
      })}
    </div>
  );
}

export function RadioPage() {
  const s = useApp();
  const systems = dashSystems(s);
  const multi = systems.length > 1;
  if (!systems.length) return <p className="empty">No systems set up yet.</p>;
  // Several systems and none picked: each on its own.
  if (!s.path[0] && multi) {
    return (
      <div className="page">
        {!running(s) && (
          <p className="muted small">
            <Light level="idle" /> Not recording — showing past data.
          </p>
        )}
        <AllSystems s={s} systems={systems} />
      </div>
    );
  }
  const name = s.path[0] ?? systems[0].name;
  const sys = systems.find((x) => x.name === name);
  if (!sys) return <p className="empty">No system called {name}.</p>;
  const [, what, key] = s.path;
  // The other systems, beside the way back to them all (as Decode's sites are).
  const picker = multi && (
    <div className="filter-row" role="tablist" aria-label="System">
      {systems.map((x) => (
        <button key={x.name} className={`btn ghost small${x.name === sys.name ? " on" : ""}`} onClick={() => setView("radio", x.name)}>
          <SystemTag config={s.config} name={x.name} health={systemRadioHealth(s, x)} />
        </button>
      ))}
    </div>
  );
  return (
    <div className="page">
      {/* (A talkgroup's or radio's page goes back to its system; a system's, with others, to them all.) */}
      {what && key ? (
        <BackBar to={sys.name} onBack={() => setView("radio", sys.name)} />
      ) : (
        multi && (
          <BackBar to="All systems" onBack={() => setView("radio")}>
            {picker}
          </BackBar>
        )
      )}
      <SystemHeader s={s} sys={sys} />
      {!running(s) && (
        <p className="muted small">
          <Light level="idle" /> Not recording — showing past data.
        </p>
      )}
      {what === "tg" && key ? <TgView s={s} sys={sys} tg={Number(key)} /> : what === "unit" && key ? <UnitView s={s} sys={sys} unit={Number(key)} /> : <SystemView s={s} sys={sys} focus={what} />}
    </div>
  );
}
