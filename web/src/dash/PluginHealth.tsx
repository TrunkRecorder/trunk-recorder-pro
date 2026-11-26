// How the plugins are doing: each one's results minute by minute, its
// queue, how long uploads take, the services it talks to (up, degraded,
// down), and its process (restarts, events it was too slow for). The
// plugins' own management (install, settings) is the second tab.

import { useState, type ReactNode } from "react";
import { pluginOn, setView, useApp, type AppState } from "../controller.ts";
import { Card, Choice, Fig, Figs, LiveSpark, Light, Stat, StackedBars, TimeSeries } from "../charts.tsx";
import { ago, bytes, compact, dur, ms, num, pct } from "../fmt.ts";
import type { PluginInfo, SeriesData } from "../protocol.ts";
import { ENDPOINT_LEVEL, K, perIn, perLive, pluginHealth, recentAvg, total, useHistory } from "./data.ts";

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

/** The highest a series reached (a gauge's per-step highs). */
function highest(d: SeriesData | undefined): number | null {
  const v = (d?.hi ?? d?.v ?? []).filter((x): x is number => x !== null);
  return v.length ? Math.max(...v) : null;
}

/**
 * How long its calls took to get there (the recorder's delivery timing, the same for every
 * plugin): from a call's end to the plugin's success, and its parts (held by delay rules,
 * waiting, the plugin's own upload). Averages over the last 5 minutes when there were
 * deliveries then, else the hour, from the running totals; the hour's longest.
 */
function useDelivery(id: string) {
  const parts = ["delivered", "ageMs", "heldMs", "uploads", "uploadMs", "waitMs"] as const;
  const hour = useHistory([...parts, "age"].map((k) => K.plg(id, k)), "1h", 60);
  const recent = recentAvg(K.plg(id, "delivered"), 300) ?? 0;
  const per = (sum: (typeof parts)[number], of: "delivered" | "uploads") =>
    recent > 0 ? perLive(K.plg(id, sum), K.plg(id, of), 300) : perIn(hour, K.plg(id, sum), K.plg(id, of));
  const delivered = total(hour?.[K.plg(id, "delivered")]) ?? 0;
  return {
    span: recent > 0 ? "5 min" : "hour",
    delivered: Math.round(delivered),
    age: per("ageMs", "delivered"),
    held: per("heldMs", "delivered"),
    wait: per("waitMs", "uploads"),
    upload: per("uploadMs", "uploads"),
    longest: highest(hour?.[K.plg(id, "age")]),
  };
}

function PluginCard({ s, p }: { s: AppState; p: PluginInfo }) {
  const on = pluginOn(s.config, p.id);
  const h = pluginHealth(p, on);
  const r = p.runtime;
  const m = r.metrics;
  const [open, setOpen] = useState(false);
  const hist = useHistory([K.plg(p.id, "ok"), K.plg(p.id, "failed"), K.plg(p.id, "queued"), K.plg(p.id, "age")], "24h", 96);
  const d = useDelivery(p.id);
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
            <Stat label="Sent" value={compact(r.ok)} sub={<Figs inline items={[{ k: "Skipped", v: compact(r.skipped) }, !!m?.bytesSent && { k: "Data", v: bytes(m.bytesSent) }]} />} hint="This run" />
            <Stat
              label="Failed"
              value={compact(r.failed)}
              level={r.failed > 0 ? "warn" : undefined}
              sub={tries ? <Figs inline items={[{ k: "Of tries", v: pct((100 * r.failed) / tries, 1), level: r.failed > 0 ? "warn" : undefined }]} /> : "none tried yet"}
              hint={r.lastFailure || undefined}
            />
            <Stat
              label="Waiting"
              value={m?.queued ?? "—"}
              level={(m?.queued ?? 0) > 20 ? "warn" : undefined}
              sub={m?.retrying ? <Figs inline items={[{ k: "To retry", v: m.retrying, level: "warn" }]} /> : m ? "queue clear" : "not reported"}
              spark={m?.queued !== undefined ? <LiveSpark k={K.plg(p.id, "queued")} color="var(--series-4)" lo={0} sinceS={600} /> : undefined}
            />
            <Stat
              label="Age at delivery"
              value={d.age !== null ? ms(d.age) : "—"}
              sub={
                d.age === null ? (
                  "none in the last hour"
                ) : (
                  <>
                    <span className="muted">{d.span === "5 min" ? "last 5 min" : "last hour"}</span>
                    <Figs
                      items={[
                        !!d.held && { k: "Held", v: ms(d.held) },
                        d.wait !== null && { k: "Waiting", v: ms(d.wait) },
                        d.upload !== null && { k: "Upload", v: ms(d.upload) },
                        d.longest !== null && { k: "Longest", v: ms(d.longest), title: "The hour's longest" },
                      ]}
                    />
                  </>
                )
              }
              spark={<LiveSpark k={K.plg(p.id, "age")} color="var(--series-1)" lo={0} sinceS={3600} />}
              hint={`From a call's end to ${p.manifest?.name ?? p.id}'s success, by this computer's clock: held by delay rules, waiting (files, its queue, failed tries) and its upload.${d.age !== null && d.upload === null ? " It doesn't say its own upload time (a plugin built on SDK 0.6 or later does), so only the age is known." : ""} Measured here; no service reports it.`}
            />
          </div>
          <div className="minutes">
            <StackedBars parts={minutes} colors={["var(--good)", "var(--muted-mark)", "var(--critical)"]} labels={["sent", "skipped", "failed"]} h={44} titles={minutes.map((c, i) => `${60 - i} min ago: ${c[0]} sent, ${c[1]} skipped, ${c[2]} failed`)} />
            <span className="muted small">last hour, per minute</span>
          </div>
          {m?.endpoints && m.endpoints.length > 0 && (
            <ul className="line-list">
              {m.endpoints.map((e) => (
                <li key={e.name}>
                  <Light level={ENDPOINT_LEVEL[e.state]} title={`${e.state}: down after 3 failures in a row, or 5 min without success`} />
                  <b>{e.name}</b>
                  <Fig v={e.state} level={ENDPOINT_LEVEL[e.state]} title="Down after 3 failures in a row, or 5 min without success" />
                  {e.latencyMs !== undefined && <Fig v={num(e.latencyMs / 1000, 1)} u="s" title="Latency" />}
                  {e.state !== "up" && e.lastOk ? <span className="muted small">last success {ago(e.lastOk)}</span> : null}
                  {e.lastError ? <span className="warn small">{e.lastError}</span> : null}
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
          <Figs
            items={[
              { k: "Last result", v: ago(r.lastResult ?? null) },
              { k: "Last success", v: ago(r.lastOk ?? null) },
              !!r.lastFail && { k: "Last failure", v: ago(r.lastFail), level: "warn" },
              { k: "Up", v: r.uptimeS ? dur(r.uptimeS) : "—" },
              !!r.restarts && { k: "Restarts", v: r.restarts, level: "warn" },
              !!r.dropped && { k: "Fell behind", v: compact(r.dropped), u: "events", level: "warn", title: "Events it was too slow to take" },
            ]}
          />
          {r.lastFailure && <p className="why why-warn small">Last failure: {r.lastFailure}</p>}
          <button className="card-more" onClick={() => setOpen(!open)} aria-expanded={open}>
            {open ? "Less ▴" : "Last 24 h and log ▾"}
          </button>
          {open && (
            <div className="card-detail stack">
              <TimeSeries
                lines={[
                  { label: "Sent a minute", color: "var(--good)", data: hist?.[K.plg(p.id, "ok")] ?? null, mul: 60 },
                  { label: "Failed a minute", color: "var(--critical)", data: hist?.[K.plg(p.id, "failed")] ?? null, mul: 60, band: true },
                ]}
                zero
                height={130}
              />
              {hist?.[K.plg(p.id, "queued")] && <TimeSeries lines={[{ label: "Waiting", color: "var(--series-4)", data: hist[K.plg(p.id, "queued")], band: true }]} zero height={110} />}
              {hist?.[K.plg(p.id, "age")] && (
                <TimeSeries lines={[{ label: "Age at delivery, s", color: "var(--series-1)", data: hist[K.plg(p.id, "age")], band: true, mul: 0.001 }]} zero height={110} fmt={(x) => x.toFixed(1)} />
              )}
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
        </>
      )}
    </div>
  );
}
