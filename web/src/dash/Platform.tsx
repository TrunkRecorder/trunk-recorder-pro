// The computer the recorder runs on: CPU (all of it, and ours), memory and
// its pressure, the disks the calls go to (and how long they'll last at
// today's rate), the network (what the recorder reaches: the hosts it
// checks, minute by minute, and the services the plugins use), and the
// machine itself — container limits included. What this platform can't
// tell is listed, greyed, with why.

import { useState } from "react";
import type { ClockInfo, MonitorSettings, PlatformInfo, SeriesData } from "../protocol.ts";
import { updateConfig, useApp, useTopic, type StatsRange } from "../controller.ts";
import { Card, Choice, Fig, Figs, HeatStrip, Info, LiveSpark, Meter, Ribbon, Stat, TimeSeries, type Level } from "../charts.tsx";
import { ago, bytes, clockAt, dur, ms, num, pct } from "../fmt.ts";
import { connections, connectionsSummary, platformHealth, total, useHistory, useRanges } from "./data.ts";
import { Field, NumberInput, TextArea, TextInput } from "../inputs.tsx";
import { Switch } from "../SetupLayout.tsx";
import { StorageView } from "./Storage.tsx";

const PRESSURE = ["normal", "warning", "critical"];

export function PlatformPage() {
  const s = useApp();
  const [range, setRange] = useState<StatsRange>("24h");
  const [part, setPart] = useState<"computer" | "storage">("computer");
  const ranges = useRanges();
  useTopic("platform");
  const hist = useHistory(["plat/cpu", "plat/proCpu", "plat/mem", "net/up", "net/rtt", "net/rx", "net/tx", "plat/temp", "clock/offsetMs"], range, 360);
  const week = useHistory(["all/audioBytes"], "7d", 7);
  const h = s.host;
  if (!h) return <p className="empty">Waiting for the first sample…</p>;
  const v = h.values;
  const p = h.platform;
  const health = platformHealth(s);
  const checks = s.config?.monitor?.networkChecks ?? true;
  const links = connections(s);
  const net = connectionsSummary(links);
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
  const store = p.store;
  const parts = store && (
    <div className="listen-tabs platform-tabs" role="tablist">
      <button role="tab" aria-selected={part === "computer"} className={part === "computer" ? "on" : ""} onClick={() => setPart("computer")}>
        Computer
      </button>
      <button role="tab" aria-selected={part === "storage"} className={part === "storage" ? "on" : ""} onClick={() => setPart("storage")}>
        Storage
      </button>
    </div>
  );
  if (store && part === "storage")
    return (
      <div className="page">
        {parts}
        <StorageView store={store} />
      </div>
    );
  return (
    <div className="page">
      {parts}
      {health.level !== "ok" && <p className={`why why-${health.level}`}>{health.why}</p>}
      <SpotlightNote spot={p.spotlight ?? null} />
      <div className="kpis">
        <Stat
          size="hero"
          label="CPU"
          value={pct(v["plat/cpu"])}
          sub={p.cpuLimitCores ? <><Fig v={num(p.cpuLimitCores, 1)} u="cores" /> container limit</> : <Fig v={p.cores} u="cores" />}
          spark={<LiveSpark k="plat/cpu" lo={0} hi={100} />}
          level={(v["plat/cpu"] ?? 0) > 90 ? "warn" : undefined}
        />
        <Stat
          label="Trunk Recorder Pro"
          value={pct(v["plat/proCpu"])}
          sub={<Figs inline items={[{ k: "Cores", v: num(v["plat/proCores"], 2) }, { k: "Memory", v: bytes(v["plat/proMem"]) }]} />}
          spark={<LiveSpark k="plat/proCpu" color="var(--series-2)" lo={0} />}
        />
        <Stat
          label="Memory"
          value={pct(v["plat/mem"])}
          sub={
            v["plat/memPsi"] !== undefined ? (
              <Figs inline items={[{ k: "Stalled", v: num(v["plat/memPsi"], 1), u: "%", level: v["plat/memPsi"] >= 10 ? "warn" : undefined }]} />
            ) : v["plat/memLevel"] !== undefined ? (
              <Figs inline items={[{ k: "Pressure", v: PRESSURE[v["plat/memLevel"]] ?? "?", level: v["plat/memLevel"] >= 2 ? "bad" : v["plat/memLevel"] >= 1 ? "warn" : undefined }]} />
            ) : (
              <Figs inline items={[{ k: "Free", v: bytes(v["plat/memAvail"]) }]} />
            )
          }
          spark={<Meter value={v["plat/mem"] ?? null} max={100} zones={[85, 95]} label="memory used" />}
          hint="Stalled: share of time spent waiting for memory."
        />
        <Stat
          label="Connections"
          value={net?.value ?? "—"}
          level={net && net.level !== "ok" ? net.level : undefined}
          sub={!net ? (links.length ? "not tried yet" : "nothing to check") : net.value === "Up" && v["net/rtt"] !== undefined ? <Figs inline items={[{ k: "Connect", v: num(v["net/rtt"], 0), u: "ms" }]} /> : net.why}
          hint={!net && !links.length ? "No host checked and no plugin sending" : undefined}
          spark={<LiveSpark k="net/rtt" color="var(--series-3)" lo={0} />}
        />
        {v["plat/temp"] !== undefined ? (
          <Stat label="Temperature" value={num(v["plat/temp"], 0)} unit="°C" level={v["plat/temp"] > 80 ? "warn" : undefined} sub={v["plat/piThrottled"] ? <Fig v="throttled" level="warn" /> : "hottest sensor"} />
        ) : (
          <Stat label="Temperature" value="—" sub={missing("temp") ?? "not reported"} />
        )}
      </div>
      <div className="row">
        <h2 className="section-title">Over time</h2>
        <span className="spacer" />
        <Choice value={range} options={ranges} onChange={setRange} label="Span" />
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
                    {spool && <Info>Calls waiting for plugins to upload, held in memory; overflows to disk.</Info>}
                    <span className="muted small mono">{d.path}</span>
                  </div>
                  <Meter value={d.missing ? null : used} max={100} zones={spool ? [75, 90] : [90, 95]} label={`${d.name} disk used`} />
                  {d.missing ? (
                    <p className="bad small">Gone: calls go to the recordings folder</p>
                  ) : (
                    <Figs
                      items={[
                        { k: "Free", v: bytes(d.freeBytes), level: used >= (spool ? 90 : 95) ? "bad" : used >= (spool ? 75 : 90) ? "warn" : undefined },
                        { k: "Size", v: bytes(d.totalBytes) },
                        !spool && { k: "Mount", v: d.mount },
                        spool && { k: "Kind", v: d.kind === "RAM disk" ? "RAM disk" : d.kind === "tmpfs" ? "/dev/shm" : d.kind === "service" ? "service folder" : "your folder" },
                        days !== null && { k: "Room for", v: days > 365 ? "1 y+" : dur(days * 86400), level: days < 7 ? "warn" : undefined, title: "At last week's average, before M4A or clean-up" },
                        days !== null && { k: "Per day", v: bytes(audioPerDay) },
                        spool && !!d.overflowed && { k: "Overflowed", v: num(d.overflowed, 0), u: d.overflowed === 1 ? "call" : "calls", level: "warn", title: "Calls that didn't fit and went to disk" },
                        spool && !!d.removed && { k: "Deleted", v: num(d.removed, 0), u: d.removed === 1 ? "time" : "times", level: "warn", title: "Deleted while running" },
                      ]}
                    />
                  )}
                </div>
              );
            })}
          </div>
        )}
      </Card>
      <Card
        title="Network"
        actions={
          s.config && (
            <Switch
              label="Checks"
              title="The hosts checked (monitor.checkHosts) and the clock's time server (monitor.timeServer). Off: neither is asked."
              checked={checks}
              onChange={(on) => updateConfig((x) => void (x.monitor && (x.monitor.networkChecks = on)))}
            />
          )
        }
      >
        {!checks ? (
          <p className="empty" title="monitor.networkChecks">Checks off: no hosts, no time server</p>
        ) : upCells.length > 0 ? (
          <>
            <Ribbon cells={upCells} titles={(up?.v ?? []).map((x, i) => `${clockAt(up!.t0 + i * up!.stepS)}: ${x === null ? "no data" : x >= 1 ? "up" : `${Math.round((x ?? 0) * 100)}% of checks answered`}`)} h={18} label="the hosts checked, answering" />
            <Figs inline items={[{ k: "Stretches with failures", v: downSteps, level: downSteps ? "warn" : undefined }]} />
          </>
        ) : (
          <p className="empty">
            No hosts to check<Info>No history kept. The plugins' services are listed as they answer.</Info>
          </p>
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
        {links.length > 0 && (
          <div className="table-wrap">
            <table className="calls">
              <thead>
                <tr>
                  <th>Connection</th>
                  <th>Now</th>
                  <th title="A host checked: how long it takes to connect. A plugin's service: how long a send takes.">Time</th>
                  <th>Last answered</th>
                  <th>Failed</th>
                </tr>
              </thead>
              <tbody>
                {links.map((x) => (
                  <tr key={`${x.plugin ?? ""}/${x.name}`}>
                    <td>
                      <span className={x.plugin ? "" : "mono"}>{x.name}</span> <span className="muted small">{x.plugin ? `· ${x.plugin}` : "· checked"}</span>
                    </td>
                    <td className={x.level === "bad" ? "bad" : x.level === "warn" ? "warn" : ""}>{x.state}</td>
                    <td className="mono">{x.ms !== null ? (x.ms >= 1000 ? `${num(x.ms / 1000, 1)} s` : `${num(x.ms, 0)} ms`) : "—"}</td>
                    <td className="small">{ago(x.lastOk)}</td>
                    <td className="mono">{x.failed ?? "—"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        {v["net/dns"] !== undefined && <Figs inline items={[{ k: "Name lookup", v: num(v["net/dns"], 0), u: "ms" }]} />}
        {s.config?.monitor && <NetworkSettings m={s.config.monitor} />}
      </Card>
      <ClockCard c={p.clock} offsets={hist?.["clock/offsetMs"] ?? null} osMissing={missing("clockSynced")} checksOff={!checks} />
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

/** A host the checks can connect to: `name:port`, `1.2.3.4:port` or `[v6]:port`. */
const HOST_PORT = /^(\[[0-9a-fA-F:.]+\]|[^\s:/[\]]+):\d{1,5}$/;

/** What the recorder itself sends to see the network answers: hosts to check, how often, and the time server. Each applies at once. */
function NetworkSettings({ m }: { m: MonitorSettings }) {
  const bad = m.checkHosts.filter((h) => !HOST_PORT.test(h));
  const set = (f: (x: MonitorSettings) => void) => updateConfig((c) => void (c.monitor && f(c.monitor)));
  return (
    <div className="grid2">
      <Field
        label="Hosts to check"
        wide
        hint={
          bad.length ? (
            <span className="bad-text">Not host:port: {bad.join(", ")}</span>
          ) : (
            <>
              host:port per line, e.g. your router
              <Info>Connected to (TCP) to keep the network's history. None: no traffic of the recorder's own.</Info>
            </>
          )
        }
      >
        <TextArea className="mono" rows={2} value={m.checkHosts.join("\n")} placeholder={"192.168.1.1:443"} onChange={(t) => set((x) => void (x.checkHosts = t.split(/[\s,]+/).filter(Boolean)))} />
      </Field>
      <Field label="Check every" hint="seconds, 5 at least">
        <NumberInput label="Check every, s" narrow value={m.checkEveryS} onChange={(n) => set((x) => void (x.checkEveryS = Math.max(5, Math.round(n ?? 15))))} />
      </Field>
      <Field
        label="Time server"
        hint={
          <>
            Empty: clock not measured<Info>Asked every 15 minutes how far off the clock is (SNTP, UDP 123).</Info>
          </>
        }
      >
        <TextInput className="mono" value={m.timeServer} placeholder="pool.ntp.org" onChange={(t) => set((x) => void (x.timeServer = t))} />
      </Field>
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
  const files = /(\d+)/.exec(spot.why)?.[1];
  return (
    <div className="banner spotlight-note" role="note">
      <div>
        <div className="row">
          <b>{spot.state === "indexed" ? "Spotlight indexes recordings" : "Spotlight may index recordings"}</b>
          <Info>Extra disk writes for files nobody searches ({spot.why}). Apps can't change Spotlight's list; rechecked daily.</Info>
          {files && <Figs inline items={[{ k: "Files", v: Number(files).toLocaleString("en-US"), level: "warn" }]} />}
        </div>
        <ol className="steps" aria-label="To exclude it">
          <li>
            <b>System Settings → Spotlight</b>
          </li>
          <li>
            <b>Search Privacy</b>
          </li>
          <li>
            <b>+</b> <span className="mono">{spot.path}</span>
            <Info>In the folder dialog, ⇧⌘G takes a path.</Info>
          </li>
        </ol>
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
function ClockCard({ c, offsets, osMissing, checksOff }: { c: ClockInfo; offsets: SeriesData | null; osMissing?: string; checksOff?: boolean }) {
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
            m ? (
              <>
                {off!.way === "behind" ? "behind" : "ahead of"} {c.server} · {ago(m.at)}
                {m.current ? null : <> · <Fig v="stale" level="warn" title="Too long ago to go by" /></>}
              </>
            ) : c.server ? (
              c.error ?? `asking ${c.server}…`
            ) : checksOff ? (
              "not asked: checks off"
            ) : (
              <span title="monitor.timeServer">no time server set</span>
            )
          }
          hint="This computer's clock less the time server's, from one SNTP query every 15 minutes. Measured here: no service reports it."
        />
        <Stat
          label="Operating system"
          value={c.os ? (c.os.synced ? "synchronized" : "not synchronized") : "—"}
          level={c.os && !c.os.synced ? "warn" : undefined}
          sub={c.os ? <Figs inline items={[{ k: "Error bound", v: ms(c.os.maxErrorMs), title: "By its own account" }]} /> : osMissing ?? "not read yet"}
          hint="What the time service keeping the clock (chrony, systemd-timesyncd, ntpd, macOS) tells the kernel. Its error bound is loose."
        />
        <Stat label="Round trip" value={m ? num(m.delayMs, 0) : "—"} unit={m ? "ms" : undefined} sub={m ? <Figs inline items={[{ k: "Stratum", v: m.stratum }]} /> : undefined} hint="The query's round trip, less the server's own time: the offset is good to about half of it." />
      </div>
      {c.service && (
        <Figs
          items={[
            { k: "Kept by", v: c.service.name },
            !!c.service.server && { k: "From", v: c.service.server },
            !!c.service.stratum && { k: "Stratum", v: c.service.stratum },
            !!c.service.lastSync && { k: "Last synced", v: ago(c.service.lastSync) },
          ]}
        />
      )}
      {c.driftMs !== null && (
        <Figs
          items={[
            {
              k: "Recording's own clock",
              v: Math.abs(c.driftMs) < 1 ? "in step" : `${offsetText(c.driftMs).by} ${offsetText(c.driftMs).way === "behind" ? "behind" : "ahead"}`,
              title: "The recording runs on its own clock, which never jumps: the clock at Start, carried on by a monotonic clock (on macOS, one that runs at the crystal's rate, uncorrected). Call times are corrected to the clock as it is now all the same; this is only how far the two have parted.",
            },
          ]}
        />
      )}
      {c.radio.length > 0 && (
        <>
          <div className="row">
            <b>Radio systems' clocks</b>
            <Info>Each system's broadcast time against the calls' time.</Info>
          </div>
          <ul className="line-list" aria-label="Radio systems' clocks">
            {c.radio.map((r) => (
              <li key={r.site}>
                <b>{r.site}</b>{" "}
                {/* (offsetMs: the system's time less the calls': positive, the system's is ahead.) */}
                {r.sane ? (
                  <>
                    <Fig v={ms(r.offsetMs)} /> <span className="muted">{r.offsetMs > 0 ? "ahead" : "behind"}</span>
                  </>
                ) : (
                  <span className="muted" title="Set to another time (its local time?)">not compared</span>
                )}
                {r.sane && !r.valid ? (
                  <>
                    {" "}
                    <Fig v="marked not valid" level="warn" title="The system marks its time not valid: shown, not gone by" />
                  </>
                ) : null}
                <span className="muted small"> · {ago(r.at)}</span>
              </li>
            ))}
          </ul>
        </>
      )}
      <TimeSeries lines={[{ label: `Offset from ${c.server ?? "the time server"}, ms (+ behind)`, color: "var(--series-1)", data: offsets }]} height={110} fmt={(x) => x.toFixed(0)} />
      <div className="row">
        {c.check && c.check.fails > 0 && (
          <span className="fig-info">
            <Figs inline items={[{ k: "Unanswered", v: `${c.check.fails} of ${c.check.tries}`, level: "warn" }]} />
            <Info>
              {c.error ? `Last: ${c.error}. ` : ""}Some networks block NTP (UDP port 123); the hosts checked don't count it.
            </Info>
          </span>
        )}
        <span className="spacer" />
        <span className="fig-info">
          <Figs inline items={[{ k: "Warns at", v: ms(c.warnMs), title: "monitor.clockWarnMs" }]} />
          <Info>The recorder only watches the clock: the operating system's time service keeps it.</Info>
        </span>
      </div>
    </Card>
  );
}
