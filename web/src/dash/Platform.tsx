// The computer the recorder runs on: CPU (all of it, and ours), memory and
// its pressure, the disks the calls go to (and how long they'll last at
// today's rate), the network (whether the internet answers, minute by
// minute), and the machine itself — container limits included. What this
// platform can't tell is listed, greyed, with why.

import { useState } from "react";
import type { PlatformInfo } from "../protocol.ts";
import { useApp, useTopic, type StatsRange } from "../controller.ts";
import { Card, Choice, HeatStrip, LiveSpark, Meter, Ribbon, Stat, TimeSeries, type Level } from "../charts.tsx";
import { ago, bytes, clockAt, dur, num, pct } from "../fmt.ts";
import { platformHealth, total, useHistory } from "./data.ts";

const RANGES: { v: StatsRange; label: string }[] = [
  { v: "1h", label: "1 h" },
  { v: "24h", label: "24 h" },
  { v: "7d", label: "7 days" },
];

const PRESSURE = ["normal", "warning", "critical"];

export function PlatformPage() {
  const s = useApp();
  const [range, setRange] = useState<StatsRange>("24h");
  useTopic("platform");
  const hist = useHistory(["plat/cpu", "plat/proCpu", "plat/mem", "net/up", "net/rtt", "net/rx", "net/tx", "plat/temp"], range, 360);
  const week = useHistory(["all/audioBytes"], "7d", 7);
  const h = s.host;
  if (!h) return <p className="empty">Waiting for the first sample…</p>;
  const v = h.values;
  const p = h.platform;
  const health = platformHealth(s);
  const missing = (field: string) => p.unavailable.find((u) => u.field === field)?.why;
  const audioPerDay = (() => {
    const t = total(week?.["all/audioBytes"]);
    const d = week?.["all/audioBytes"];
    const covered = d ? d.v.filter((x) => x !== null).length * d.stepS : 0;
    return t !== null && covered >= 6 * 3600 ? (t / covered) * 86400 : null;
  })();
  const up = hist?.["net/up"];
  const upCells: (Level | null)[] = (up?.v ?? []).map((x, i) => (x === null ? null : (up!.lo[i] ?? x) >= 1 ? "ok" : x > 0 ? "warn" : "bad"));
  const downSteps = upCells.filter((c) => c === "warn" || c === "bad").length;
  return (
    <div className="page">
      {health.level !== "ok" && <p className={`why why-${health.level}`}>{health.why}</p>}
      <SpotlightNote spot={p.spotlight ?? null} />
      <div className="kpis">
        <Stat
          size="hero"
          label="CPU"
          value={pct(v["plat/cpu"])}
          sub={p.cpuLimitCores ? `of the container's ${num(p.cpuLimitCores, 1)} cores` : `of ${p.cores} cores`}
          spark={<LiveSpark k="plat/cpu" lo={0} hi={100} />}
          level={(v["plat/cpu"] ?? 0) > 90 ? "warn" : undefined}
        />
        <Stat
          label="Trunk Recorder Pro"
          value={pct(v["plat/proCpu"])}
          sub={`${num(v["plat/proCores"], 2)} cores · ${bytes(v["plat/proMem"])} memory`}
          spark={<LiveSpark k="plat/proCpu" color="var(--series-2)" lo={0} />}
        />
        <Stat
          label="Memory"
          value={pct(v["plat/mem"])}
          sub={
            v["plat/memPsi"] !== undefined
              ? `stalled ${num(v["plat/memPsi"], 1)}% of the time`
              : v["plat/memLevel"] !== undefined
                ? `pressure ${PRESSURE[v["plat/memLevel"]] ?? "?"}`
                : `${bytes(v["plat/memAvail"])} available`
          }
          spark={<Meter value={v["plat/mem"] ?? null} max={100} zones={[85, 95]} label="memory used" />}
          hint="Stalled: time spent waiting for memory."
        />
        <Stat
          label="Internet"
          value={v["net/up"] === undefined ? "—" : v["net/up"] >= 1 ? "Up" : v["net/up"] > 0 ? "Partly" : "Down"}
          level={v["net/up"] === undefined ? undefined : v["net/up"] >= 1 ? undefined : v["net/up"] > 0 ? "warn" : "bad"}
          sub={v["net/rtt"] !== undefined ? `${num(v["net/rtt"], 0)} ms to connect` : "no checks set up"}
          spark={<LiveSpark k="net/rtt" color="var(--series-3)" lo={0} />}
        />
        {v["plat/temp"] !== undefined ? (
          <Stat label="Temperature" value={num(v["plat/temp"], 0)} unit="°C" level={v["plat/temp"] > 80 ? "warn" : undefined} sub={v["plat/piThrottled"] ? "throttled now" : "hottest sensor"} />
        ) : (
          <Stat label="Temperature" value="—" sub={missing("temp") ?? "not reported"} />
        )}
      </div>
      <div className="row">
        <h2 className="section-title">Over time</h2>
        <span className="spacer" />
        <Choice value={range} options={RANGES} onChange={setRange} label="Span" />
      </div>
      <div className="columns">
        <Card title="CPU">
          <TimeSeries
            lines={[
              { label: "Everything", color: "var(--series-1)", data: hist?.["plat/cpu"] ?? null, band: true },
              { label: "Trunk Recorder Pro", color: "var(--series-2)", data: hist?.["plat/proCpu"] ?? null },
            ]}
            zero
            hi={100}
            fmt={(x) => `${x.toFixed(0)}%`}
          />
          {p.perCore && (
            <div className="cores">
              <HeatStrip v={p.perCore} max={100} titles={p.perCore.map((c, i) => `core ${i}: ${c}%`)} h={16} label="each core" />
              <span className="muted small">each core now</span>
            </div>
          )}
        </Card>
        <Card title="Memory">
          <TimeSeries lines={[{ label: "Used", color: "var(--series-1)", data: hist?.["plat/mem"] ?? null, band: true }]} zero hi={100} fmt={(x) => `${x.toFixed(0)}%`} />
          <div className="kv small">
            <span>
              Total <b>{bytes(p.memTotal)}</b>
            </span>
            <span>
              Available <b>{bytes(v["plat/memAvail"])}</b>
            </span>
            {v["plat/swap"] !== undefined && (
              <span>
                Swap used <b>{pct(v["plat/swap"])}</b>
              </span>
            )}
          </div>
        </Card>
      </div>
      <Card title="Disks">
        {p.disks.length === 0 ? (
          <p className="empty">{missing("disk") ?? "No disks found."}</p>
        ) : (
          <div className="disk-list">
            {p.disks.map((d) => {
              const used = 100 - (100 * d.freeBytes) / Math.max(1, d.totalBytes);
              const days = d.name === "recordings" && audioPerDay ? d.freeBytes / audioPerDay : null;
              const spool = d.name === "spool";
              return (
                <div key={d.name} className="disk">
                  <div className="row">
                    <b>{spool ? "RAM spool" : d.name === "recordings" ? "Recordings" : "App data"}</b>
                    <span className="muted small mono">{d.path}</span>
                  </div>
                  <Meter value={used} max={100} zones={spool ? [75, 90] : [90, 95]} label={`${d.name} disk used`} />
                  <div className="kv small">
                    <span>
                      <b>{bytes(d.freeBytes)}</b> free of {bytes(d.totalBytes)}
                    </span>
                    {spool ? (
                      <span title="Held in memory until plugins upload; overflows to disk.">
                        {d.kind === "RAM disk" ? "a RAM disk" : d.kind === "tmpfs" ? "in /dev/shm" : "a folder of yours"} · calls waiting for upload
                      </span>
                    ) : (
                      <span>on {d.mount}</span>
                    )}
                    {spool && !!d.overflowed && <span className="warn">{d.overflowed} call{d.overflowed === 1 ? "" : "s"} overflowed to disk</span>}
                    {days !== null && (
                      <span className={days < 7 ? "warn" : ""} title="At last week's average, before M4A or clean-up">
                        About <b>{days > 365 ? "a year or more" : dur(days * 86400)}</b> of recordings left at {bytes(audioPerDay)} a day
                      </span>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </Card>
      <Card title="Network">
        {upCells.length > 0 ? (
          <>
            <Ribbon cells={upCells} titles={(up?.v ?? []).map((x, i) => `${clockAt(up!.t0 + i * up!.stepS)}: ${x === null ? "no data" : x >= 1 ? "up" : `${Math.round((x ?? 0) * 100)}% of checks answered`}`)} h={18} label="internet reachability" />
            <p className="muted small">
              {downSteps ? `${downSteps} stretch${downSteps > 1 ? "es" : ""} with failed checks` : "Every check answered"}
            </p>
          </>
        ) : (
          <p className="empty">No checks set up (monitor.probeHosts).</p>
        )}
        <div className="columns">
          <TimeSeries lines={[{ label: "Connect time, ms", color: "var(--series-3)", data: hist?.["net/rtt"] ?? null, band: true }]} zero height={120} fmt={(x) => x.toFixed(0)} />
          <TimeSeries
            lines={[
              { label: "Received", color: "var(--series-1)", data: hist?.["net/rx"] ?? null },
              { label: "Sent", color: "var(--series-2)", data: hist?.["net/tx"] ?? null },
            ]}
            zero
            height={120}
            fmt={(x) => `${bytes(x)}/s`}
          />
        </div>
        {p.probes.length > 0 && (
          <div className="table-wrap">
            <table className="calls">
              <thead>
                <tr>
                  <th>Check</th>
                  <th>Now</th>
                  <th>Connect</th>
                  <th>Last answered</th>
                  <th>Failed</th>
                </tr>
              </thead>
              <tbody>
                {p.probes.map((x) => (
                  <tr key={x.target}>
                    <td className="mono">{x.target}</td>
                    <td>{x.ok === null ? "—" : x.ok ? "answers" : `down ${x.downSince ? `for ${dur(Date.now() / 1000 - x.downSince)}` : ""}`}</td>
                    <td className="mono">{x.rttMs !== null ? `${num(x.rttMs, 0)} ms` : "—"}</td>
                    <td className="small">{ago(x.lastOk)}</td>
                    <td className="mono">
                      {x.fails} of {x.tries}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        {v["net/dns"] !== undefined && <p className="muted small">Name lookups take {num(v["net/dns"], 0)} ms.</p>}
      </Card>
      <Card title="This computer">
        <div className="kv">
          <span>
            <b>{p.os}</b> · {p.arch}
          </span>
          {p.host && <span>{p.host}</span>}
          <span>{p.cores} cores</span>
          <span>up {dur(p.uptimeS)}</span>
          {p.container && (
            <span className="badge-kind">
              in {p.container}
              {p.cpuLimitCores ? ` · ${num(p.cpuLimitCores, 1)} CPU limit` : ""}
            </span>
          )}
        </div>
        {p.unavailable.length > 0 && (
          <ul className="unavailable small">
            {p.unavailable.map((u) => (
              <li key={u.field}>
                <b>{u.field}</b>: {u.why}
              </li>
            ))}
          </ul>
        )}
      </Card>
    </div>
  );
}

/**
 * macOS: Spotlight reads every call saved in the recordings folder into its
 * index — disk writes for files nobody searches. Apps can't read or change
 * its exclusion list, so this says how to (shown until the check finds it
 * excluded; a "can't tell" can be dismissed).
 */
function SpotlightNote({ spot }: { spot: PlatformInfo["spotlight"] }) {
  const key = spot ? `spotlightNoted:${spot.path}` : "";
  const [noted, setNoted] = useState(() => {
    try {
      return !!key && localStorage.getItem(key) === "1";
    } catch {
      return false;
    }
  });
  if (!spot || spot.state === "excluded" || (spot.state === "unknown" && noted)) return null;
  const note = () => {
    try {
      localStorage.setItem(key, "1");
    } catch {
      // (Shown again next time.)
    }
    setNoted(true);
  };
  return (
    <div className="banner spotlight-note" role="note">
      <div>
        <b>{spot.state === "indexed" ? "Spotlight indexes the recordings folder" : "Spotlight may be indexing the recordings folder"}</b>
        <p className="small">
          Extra disk writes for files nobody searches. <span className="muted">({spot.why}.)</span>
        </p>
        <ol className="small">
          <li>
            Open <b>System Settings → Spotlight</b> and click <b>Search Privacy</b>.
          </li>
          <li>
            Click <b>+</b> and choose <span className="mono">{spot.path}</span> (in the dialog, ⇧⌘G takes a path).
          </li>
        </ol>
        <p className="muted small">Apps can't change that list; rechecked daily.</p>
      </div>
      {spot.state === "unknown" && <button onClick={note}>I've done it</button>}
    </div>
  );
}
