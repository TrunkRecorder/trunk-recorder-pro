// Platform → Storage: what the recorder's store holds and how big each part
// is — every table, its rows, and each of its indexes — measured by the
// recorder every 15 minutes, and how each has grown (the `store/<name>`
// series) — and what it writes to disk (`store/written`): what wears an SD
// card or eMMC. Settings for it are in Setup → Data.

import { Fragment, useState } from "react";
import type { PlatformInfo, StoreSizes } from "../protocol.ts";
import { setSetupTab, setView, type StatsRange } from "../controller.ts";
import { Card, Choice, Stat, TimeSeries } from "../charts.tsx";
import { bytes, num } from "../fmt.ts";
import { total, useHistory, useRanges } from "./data.ts";
import { INDEX_USES } from "../storeIndexes.ts";

/** A table with its indexes, biggest first. */
interface TableRow {
  name: string;
  rows?: number;
  bytes: number;
  indexes: { name: string; bytes: number }[];
}

/** Each table with its indexes (an index is listed under the table it indexes). */
export function tablesOf(s: StoreSizes): TableRow[] {
  const t = new Map<string, TableRow>();
  const at = (name: string) => t.get(name) ?? t.set(name, { name, bytes: 0, indexes: [] }).get(name)!;
  for (const o of s.objects) {
    const row = at(o.table);
    if (o.index) row.indexes.push({ name: o.name, bytes: o.bytes });
    else {
      row.bytes += o.bytes;
      if (o.rows !== undefined) row.rows = o.rows;
    }
  }
  const all = (r: TableRow) => r.bytes + r.indexes.reduce((n, i) => n + i.bytes, 0);
  return [...t.values()].sort((a, b) => all(b) - all(a));
}

type Registry = NonNullable<PlatformInfo["store"]>["registry"];

export function StorageView(props: { store: { backend: string; radioIndex: boolean; talkgroupIndex?: boolean; registry?: Registry; sizes: StoreSizes; written?: { bytes: number; of: "store" | "server" } | { why: string } } }) {
  const { backend, radioIndex, talkgroupIndex = true, registry, sizes, written } = props.store;
  const off = [!talkgroupIndex && "talkgroup", !radioIndex && "radio"].filter(Boolean) as string[];
  const [range, setRange] = useState<StatsRange>("7d");
  const ranges = useRanges();
  const tables = tablesOf(sizes);
  const indexBytes = sizes.objects.filter((o) => o.index).reduce((n, o) => n + o.bytes, 0);
  const tableBytes = sizes.objects.filter((o) => !o.index).reduce((n, o) => n + o.bytes, 0);
  const top = sizes.objects.slice(0, 5).map((o) => o.name);
  const hist = useHistory(["store/total", "store/written", ...top.map((n) => `store/${n}`)], range, 360);
  const day = useHistory(["store/written"], "24h", 96);
  const perDay = (() => {
    const d = day?.["store/written"];
    const t = total(d);
    const covered = d ? d.v.filter((x) => x !== null).length * d.stepS : 0;
    return t !== null && covered >= 3600 ? (t / covered) * 86400 : null;
  })();
  const measured = written && "of" in written ? written : null;
  const max = Math.max(1, ...sizes.objects.map((o) => o.bytes));
  const colors = ["var(--series-2)", "var(--series-3)", "var(--series-4)", "var(--series-5)", "var(--series-6)"];
  const toData = () => {
    setView("setup");
    setSetupTab("data");
  };
  return (
    <>
      <div className="kpis">
        <Stat size="hero" label="Store" value={bytes(sizes.fileBytes + sizes.walBytes)} sub={backend === "postgres" ? "PostgreSQL: its tables and indexes" : "SQLite, with its write-ahead log"} />
        <Stat label="Tables" value={bytes(tableBytes)} sub={`${tables.length} tables`} />
        <Stat label="Indexes" value={bytes(indexBytes)} sub={tableBytes ? `${num((100 * indexBytes) / tableBytes, 0)}% of the tables` : undefined} />
        {backend === "sqlite" && <Stat label="Write-ahead log" value={bytes(sizes.walBytes)} sub={`${bytes(sizes.freeBytes)} free inside the file`} />}
        <Stat
          label="Written"
          value={measured ? (perDay === null ? "—" : `${bytes(perDay)}`) : "—"}
          unit={measured && perDay !== null ? "a day" : undefined}
          sub={
            !written ? undefined
            : !measured ? `not measured: ${"why" in written ? written.why : ""}`
            : measured.of === "server" ? `the PostgreSQL server's log (all its databases) · ${bytes(measured.bytes)} so far`
            : perDay === null ? `${bytes(measured.bytes)} since it started; a day's rate after an hour`
            : `to disk, the last 24 h · ${bytes(measured.bytes)} since it started`
          }
          hint="What the store writes to disk: commits, checkpoints and its daily backup. It's what wears an SD card or eMMC; Setup → Data has what lowers it."
        />
      </div>
      {off.length > 0 && (
        <p className="muted small">
          Calls aren't indexed by {off.join(" or ")}: a {off.join(" or ")}'s list shows only its recent calls.{" "}
          <button className="btn ghost small" onClick={toData}>
            Setup → Data
          </button>
        </p>
      )}
      <div className="row">
        <h2 className="section-title">Growth</h2>
        <span className="spacer" />
        <Choice value={range} options={ranges} onChange={setRange} label="Span" />
      </div>
      <Card title="Size over time">
        <TimeSeries
          lines={[{ label: "Store", color: "var(--series-1)", data: hist?.["store/total"] ?? null }, ...top.map((n, i) => ({ label: n, color: colors[i], data: hist?.[`store/${n}`] ?? null }))]}
          zero
          fmt={(x) => bytes(x)}
          empty="Measured every 15 minutes: its history starts with the first."
        />
      </Card>
      {measured && (
        <Card title="Written to disk">
          <TimeSeries lines={[{ label: "Written", color: "var(--series-1)", data: hist?.["store/written"] ?? null, band: true }]} zero fmt={(x) => `${bytes(x * 86400)}/day`} empty="Its history starts as it writes." />
        </Card>
      )}
      {registry && <RegistryCard registry={registry} toData={toData} />}
      <Card title="Tables and indexes">
        <table className="calls store-sizes">
          <thead>
            <tr>
              <th>Table / index</th>
              <th className="num">Rows</th>
              <th className="num">Size</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {tables.map((t) => (
              <Fragment key={t.name}>
                <tr>
                  <td className="mono">{t.name}</td>
                  <td className="num">{t.rows === undefined ? "—" : num(t.rows, 0)}</td>
                  <td className="num">{bytes(t.bytes)}</td>
                  <td className="bar-cell">
                    <span className="bar" style={{ width: `${(100 * t.bytes) / max}%` }} />
                  </td>
                </tr>
                {t.indexes.map((i) => (
                  <tr key={`${t.name}/${i.name}`} className="sub">
                    <td className="mono muted" title={INDEX_USES[i.name] ? `Finds ${INDEX_USES[i.name].finds}: ${INDEX_USES[i.name].used}` : undefined}>
                      ↳ {i.name}
                      {INDEX_USES[i.name] && <span className="index-use"> · {INDEX_USES[i.name].finds}</span>}
                    </td>
                    <td />
                    <td className="num">{bytes(i.bytes)}</td>
                    <td className="bar-cell">
                      <span className="bar alt" style={{ width: `${(100 * i.bytes) / max}%` }} />
                    </td>
                  </tr>
                ))}
              </Fragment>
            ))}
          </tbody>
        </table>
        <p className="muted small">Measured every 15 minutes{backend === "postgres" ? "; rows are PostgreSQL's estimate" : ""}. Settings: Setup → Data.</p>
        <details className="muted small">
          <summary>What the call indexes are for</summary>
          <ul>
            {Object.entries(INDEX_USES).map(([name, u]) => (
              <li key={name}>
                <span className="mono">{name}</span> finds {u.finds}: {u.used}.{u.setting && ` Optional (Setup → Data): without it, ${u.without}.`}
              </li>
            ))}
          </ul>
        </details>
      </Card>
    </>
  );
}

/** How full the radio registry is: each system's rows against its caps (Setup → Data). */
function RegistryCard(props: { registry: Registry; toData: () => void }) {
  const { caps, systems } = props.registry;
  const of = (n: number, cap: number | null) => (cap === null ? num(n, 0) : `${num(n, 0)} of ${num(cap, 0)}`);
  const full = (n: number, cap: number | null) => cap !== null && n >= cap;
  return (
    <Card title="Radio registry">
      <table className="calls store-sizes">
        <thead>
          <tr>
            <th>System</th>
            <th className="num">Talkgroups</th>
            <th className="num">Radios</th>
            <th className="num">Pairs</th>
          </tr>
        </thead>
        <tbody>
          {systems.map((x) => (
            <tr key={x.system}>
              <td className="mono">{x.system}</td>
              <td className={`num${full(x.talkgroups, caps.talkgroups) ? " warn" : ""}`}>{of(x.talkgroups, caps.talkgroups)}</td>
              <td className={`num${full(x.radios, caps.radios) ? " warn" : ""}`}>{of(x.radios, caps.radios)}</td>
              <td className={`num${full(x.pairs, caps.pairs) ? " warn" : ""}`}>{of(x.pairs, caps.pairs)}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <p className="muted small">
        {caps.days === null ? "Kept however long ago it was heard" : `Forgotten after ${num(caps.days, 0)} days unheard`}; at a limit, the least recently heard go.{" "}
        <button className="btn ghost small" onClick={props.toData}>
          Setup → Data
        </button>
      </p>
    </Card>
  );
}
