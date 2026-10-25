// The computer the recorder runs on: CPU (all of it, and ours), memory and
// its pressure, the disks the calls go to (and how long they'll last at
// today's rate), the network (whether the internet answers, minute by
// minute), and the machine itself — container limits included. What this
// platform can't tell is listed, greyed, with why.

import { useState } from "react";
import type { ClockInfo, PlatformInfo, SeriesData } from "../protocol.ts";
import { useApp, useTopic, type StatsRange } from "../controller.ts";
import { Card, Choice, HeatStrip, LiveSpark, Meter, Ribbon, Stat, TimeSeries, type Level } from "../charts.tsx";
import { ago, bytes, clockAt, dur, ms, num, pct } from "../fmt.ts";
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
  const hist = useHistory(["plat/cpu", "plat/proCpu", "plat/mem", "net/up", "net/rtt", "net/rx", "net/tx", "plat/temp", "clock/offsetMs"], range, 360);
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
                  <Meter value={d.missing ? null : used} max={100} zones={spool ? [75, 90] : [90, 95]} label={`${d.name} disk used`} />
                  <div className="kv small">
                    {d.missing ? (
                      <span className="bad">Gone: calls go to the recordings folder</span>
                    ) : (
                      <span>
                        <b>{bytes(d.freeBytes)}</b> free of {bytes(d.totalBytes)}
                      </span>
                    )}
                    {spool ? (
                      <span title="Held in memory until plugins upload; overflows to disk.">
                        {d.kind === "RAM disk" ? "a RAM disk" : d.kind === "tmpfs" ? "in /dev/shm" : d.kind === "service" ? "in the service's runtime folder" : "a folder of yours"} · calls waiting for upload
                      </span>
                    ) : (
                      <span>on {d.mount}</span>
                    )}
                    {spool && !!d.overflowed && <span className="warn">{d.overflowed} call{d.overflowed === 1 ? "" : "s"} overflowed to disk</span>}
                    {spool && !!d.removed && <span className="warn">deleted {d.removed === 1 ? "once" : `${d.removed} times`} while running</span>}
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
      <ClockCard c={p.clock} offsets={hist?.["clock/offsetMs"] ?? null} osMissing={missing("clockSynced")} />
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

/** How far a clock is off, in words: "35 ms behind", "1.2 s ahead of" (positive: behind). */
function offsetText(v: number): { by: string; way: string } {
  return { by: ms(v), way: v > 0 ? "behind" : "ahead of" };
}

/**
 * The clock: what the operating system says of it, and what the time server measured. The
 * recorder decides what's wrong (`problem`); this only shows it.
 */
function ClockCard({ c, offsets, osMissing }: { c: ClockInfo; offsets: SeriesData | null; osMissing?: string }) {
  const m = c.measured;
  const level: Level | undefined = c.problem?.level ?? (m?.current || c.os ? "ok" : undefined);
  const off = m && offsetText(m.offsetMs);
  return (
    <Card title="Clock" level={level} why={c.problem?.why}>
      <div className="src-grid">
        <Stat
          label="Offset, measured"
          value={off ? off.by : "—"}
          sub={
            m
              ? `${off!.way} ${c.server}, ${ago(m.at)}${m.current ? "" : " (too long ago to go by)"}`
              : c.server
                ? c.error ?? `asking ${c.server}…`
                : "no time server set (monitor.timeServer)"
          }
          hint="This computer's clock less the time server's, from one SNTP query every 15 minutes. Measured here: no service reports it."
        />
        <Stat
          label="Operating system"
          value={c.os ? (c.os.synced ? "synchronized" : "not synchronized") : "—"}
          level={c.os && !c.os.synced ? "warn" : undefined}
          sub={c.os ? `within ${c.os.maxErrorMs >= 1000 ? `${num(c.os.maxErrorMs / 1000, 1)} s` : `${num(c.os.maxErrorMs, 0)} ms`} by its own account` : osMissing ?? "not read yet"}
          hint="What the time service keeping the clock (chrony, systemd-timesyncd, ntpd, macOS) tells the kernel. Its error bound is loose."
        />
        <Stat label="Round trip" value={m ? num(m.delayMs, 0) : "—"} unit={m ? "ms" : undefined} sub={m ? `stratum ${m.stratum}` : undefined} hint="The query's round trip, less the server's own time: the offset is good to about half of it." />
      </div>
      {c.service && (
        <p className="small">
          Kept by <b>{c.service.name}</b>
          {c.service.server ? <> from {c.service.server}</> : null}
          {c.service.stratum ? `, stratum ${c.service.stratum}` : ""}
          {c.service.lastSync ? `, last synchronized ${ago(c.service.lastSync)}` : ""}.
        </p>
      )}
      {c.driftMs !== null && (
        <p className={`small${c.problem?.cause === "recording" ? " warn" : ""}`} title="Calls are stamped by the clock as it was when recording started, carried on by a clock that never jumps: a correction since then doesn't reach them until the next Start.">
          Calls are stamped by the clock as it was at Start:{" "}
          {Math.abs(c.driftMs) < 1 ? "the same as the clock now" : `${offsetText(c.driftMs).by} ${offsetText(c.driftMs).way} the clock now`}.
        </p>
      )}
      {c.radio.length > 0 && (
        <>
          <p className="small">The radio systems' own clocks, against the calls' time:</p>
          <ul className="line-list small" aria-label="Radio systems' clocks">
            {c.radio.map((r) => (
              <li key={r.site}>
                <b>{r.site}</b>
                <span className="muted">
                  {/* (offsetMs: the system's time less the calls': positive, the system's is ahead.) */}
                  {r.sane ? ` ${ms(r.offsetMs)} ${r.offsetMs > 0 ? "ahead of" : "behind"} them` : " set to another time (its local time?): not compared"}
                  {r.sane && !r.valid ? " (the system marks its time not valid: shown, not gone by)" : ""}, {ago(r.at)}
                </span>
              </li>
            ))}
          </ul>
        </>
      )}
      <TimeSeries lines={[{ label: `This computer's clock against ${c.server ?? "the time server"}, ms (+: behind)`, color: "var(--series-1)", data: offsets }]} height={110} fmt={(x) => x.toFixed(0)} />
      {c.check && c.check.fails > 0 && (
        <p className="muted small">
          {c.check.fails} of {c.check.tries} queries unanswered{c.error ? ` (last: ${c.error})` : ""}. Some networks block NTP (UDP port 123); the internet checks don't count it.
        </p>
      )}
      <p className="muted small">
        The recorder only watches the clock: the operating system's time service keeps it. A warning at {ms(c.warnMs)} off (monitor.clockWarnMs).
      </p>
    </Card>
  );
}
