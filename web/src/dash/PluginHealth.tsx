// How the plugins are doing: each one's results minute by minute, its
// queue, how long uploads take, the services it talks to (up, degraded,
// down), and its process (restarts, events it was too slow for). The
// plugins' own management (install, settings) is the second tab.

import { useState, type ReactNode } from "react";
import { pluginOn, setView, useApp, type AppState } from "../controller.ts";
import { Card, Choice, Hint, LiveSpark, Light, Stat, StackedBars, TimeSeries } from "../charts.tsx";
import { ago, bytes, compact, dur, num, pct } from "../fmt.ts";
import type { PluginInfo } from "../protocol.ts";
import { K, pluginHealth, useHistory } from "./data.ts";

const ENDPOINT_LEVEL = { up: "ok", degraded: "warn", down: "bad", unknown: "idle" } as const;

/** "packetsSent" → "Packets sent", "bytes_out" → "Bytes out". */
function humanize(key: string): string {
  const words = key
    .replace(/[_-]+/g, " ")
    .replace(/([a-z\d])([A-Z])/g, "$1 $2")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1 $2")
    .trim()
    .split(/\s+/)
    .map((w) => (/^[A-Z\d]{2,}$/.test(w) ? w : w.toLowerCase()));
  const text = words.join(" ");
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/** An extra metric's value: a number compactly, text as it is, anything else as JSON. */
function extraValue(v: unknown): string {
  if (typeof v === "number") return compact(v);
  if (typeof v === "string") return v;
  if (typeof v === "boolean") return v ? "yes" : "no";
  return JSON.stringify(v);
}

function PluginCard({ s, p }: { s: AppState; p: PluginInfo }) {
  const on = pluginOn(s.config, p.id);
  const h = pluginHealth(p, on);
  const r = p.runtime;
  const m = r.metrics;
  const [open, setOpen] = useState(false);
  const hist = useHistory([K.plg(p.id, "ok"), K.plg(p.id, "failed"), K.plg(p.id, "queued"), K.plg(p.id, "latency")], "24h", 96);
  // The last hour, a column a minute: [ok, skipped, failed].
  const nowM = Math.floor(Date.now() / 60_000) * 60;
  const minutes = Array.from({ length: 60 }, (_, i) => {
    const t = nowM - (59 - i) * 60;
    return r.minutes?.find((x) => x[0] === t)?.[1] ?? [0, 0, 0];
  });
  const tries = r.ok + r.failed;
  // Its own figures (simplestream's packetsSent, packetsDropped…), as they come.
  const extras = Object.entries(m?.extra ?? {}).filter(([, v]) => v !== null && v !== undefined && v !== "");
  return (
    <Card
      title={p.manifest?.name ?? p.id}
      level={h.level}
      why={h.why}
      actions={<span className="badge-kind">{p.manifest ? `v${p.manifest.version}` : p.id}</span>}
      className="plugin-card"
    >
      {!on ? (
        <p className="empty">
          Off.{" "}
          <button className="linkish" onClick={() => setView("plugins", "manage")}>
            Turn it on
          </button>
        </p>
      ) : (
        <>
          <div className="src-grid">
            <Stat label="Sent" value={compact(r.ok)} sub={`${r.skipped} skipped this run`} />
            <Stat
              label="Failed"
              value={compact(r.failed)}
              level={r.failed > 0 ? "warn" : undefined}
              sub={tries ? `${pct((100 * r.failed) / tries, 1)} of tries` : "nothing tried yet"}
              hint={r.lastFailure || undefined}
            />
            <Stat
              label="Waiting"
              value={m?.queued ?? "—"}
              level={(m?.queued ?? 0) > 20 ? "warn" : undefined}
              sub={m?.retrying ? `${m.retrying} to retry` : m ? "queue clear" : "not reported"}
              spark={m?.queued !== undefined ? <LiveSpark k={K.plg(p.id, "queued")} color="var(--series-4)" lo={0} sinceS={600} /> : undefined}
            />
            <Stat label="Upload time" value={m?.latencyMs !== undefined ? num(m.latencyMs / 1000, 1) : "—"} unit={m?.latencyMs !== undefined ? "s" : undefined} sub={m?.bytesSent ? `${bytes(m.bytesSent)} sent` : undefined} />
          </div>
          <div className="minutes">
            <StackedBars parts={minutes} colors={["var(--good)", "var(--muted-mark)", "var(--critical)"]} labels={["sent", "skipped", "failed"]} h={44} titles={minutes.map((c, i) => `${60 - i} min ago: ${c[0]} sent, ${c[1]} skipped, ${c[2]} failed`)} />
            <span className="muted small">last hour, per minute</span>
          </div>
          {m?.endpoints && m.endpoints.length > 0 && (
            <ul className="line-list">
              {m.endpoints.map((e) => (
                <li key={e.name}>
                  <Light level={ENDPOINT_LEVEL[e.state]} title={e.state} />
                  <b>{e.name}</b>
                  <span className="muted small">
                    {e.state}
                    {e.latencyMs !== undefined ? ` · ${num(e.latencyMs / 1000, 1)} s` : ""}
                    {e.lastOk ? ` · last success ${ago(e.lastOk)}` : ""}
                    {e.lastError ? ` · ${e.lastError}` : ""}
                  </span>
                </li>
              ))}
            </ul>
          )}
          {extras.length > 0 && (
            <div className="kv small" aria-label="What the plugin reports">
              {extras.map(([k, v]) => (
                <span key={k}>
                  {humanize(k)} <b>{extraValue(v)}</b>
                </span>
              ))}
            </div>
          )}
          <div className="kv small">
            <span>
              Last result <b>{ago(r.lastResult ?? null)}</b>
            </span>
            <span>
              Last success <b>{ago(r.lastOk ?? null)}</b>
            </span>
            {r.lastFail ? (
              <span>
                Last failure <b>{ago(r.lastFail)}</b>
              </span>
            ) : null}
            <span>
              Up <b>{r.uptimeS ? dur(r.uptimeS) : "—"}</b>
            </span>
            {r.restarts ? (
              <span className="warn">
                Restarted <b>{r.restarts}×</b>
              </span>
            ) : null}
            {r.dropped ? (
              <span className="warn" title="Events it was too slow to take">
                Fell behind <b>{compact(r.dropped)}</b> events
              </span>
            ) : null}
          </div>
          {r.lastFailure && <p className="why why-warn small">Last failure: {r.lastFailure}</p>}
          <button className="card-more" onClick={() => setOpen(!open)} aria-expanded={open}>
            {open ? "Less ▴" : "Last 24 h and log ▾"}
          </button>
          {open && (
            <div className="card-detail stack">
              <TimeSeries
                lines={[
                  { label: "Sent a minute", color: "var(--good)", data: hist?.[K.plg(p.id, "ok")] ?? null, mul: 60 },
                  { label: "Failed a minute", color: "var(--critical)", data: hist?.[K.plg(p.id, "failed")] ?? null, mul: 60 },
                ]}
                zero
                height={130}
              />
              {hist?.[K.plg(p.id, "queued")] && <TimeSeries lines={[{ label: "Waiting", color: "var(--series-4)", data: hist[K.plg(p.id, "queued")], band: true }]} zero height={110} />}
              <pre className="log-lines">
                {[...r.log].reverse().map((l, i) => (
                  <div key={i} className={`k-${l.level === "error" ? "error" : "plugin"}`}>
                    <span className="muted">{new Date(l.time * 1000).toLocaleTimeString()}</span> {l.text}
                  </div>
                ))}
              </pre>
            </div>
          )}
        </>
      )}
    </Card>
  );
}

export function PluginHealthPage({ manage }: { manage: ReactNode }) {
  const s = useApp();
  const tab = s.path[0] === "manage" ? "manage" : "health";
  const plugins = s.plugins?.plugins ?? [];
  const on = plugins.filter((p) => pluginOn(s.config, p.id));
  const off = plugins.filter((p) => !pluginOn(s.config, p.id));
  return (
    <div className="page">
      <Choice
        value={tab}
        onChange={(v) => setView("plugins", ...(v === "manage" ? ["manage"] : []))}
        label="Plugins"
        options={[
          { v: "health", label: "Health" },
          { v: "manage", label: "Install & settings" },
        ]}
      />
      {tab === "manage" ? (
        manage
      ) : plugins.length === 0 ? (
        <p className="empty">No plugins installed.</p>
      ) : (
        <>
          <div className="card-grid wide">
            {on.map((p) => (
              <PluginCard key={p.id} s={s} p={p} />
            ))}
          </div>
          {off.length > 0 && <p className="muted small">Off: {off.map((p) => p.manifest?.name ?? p.id).join(", ")}</p>}
          <Hint>A service is down after 3 failures in a row, or 5 min without success.</Hint>
        </>
      )}
    </div>
  );
}
