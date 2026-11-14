// Every system at a glance: today's big numbers, a card per system (is its
// control channel decoding as usual, what's on the air), the sources, the
// plugins and the computer in a line each, and what happened lately.

import { formatMhz } from "../config.ts";
import { can, mayEdit, pluginOn, radioQuery, statsSince, setView, start, usePolled, useApp, type AppState } from "../controller.ts";
import { Card, Fig, Figs, HeatStrip, LiveSpark, Light, Meter, Sparkline, Stat, SystemTag, Trend } from "../charts.tsx";
import { ago, bytes, compact, hmm, mhz, ms, num, pct } from "../fmt.ts";
import type { FreqRow, MonitorEvent, RadioSummary } from "../protocol.ts";
import { change, connections, connectionsSummary, goTo, rateOver, dashSystems, K, KIND_LABEL, pluginHealth, platformHealth, spoolRemovedFix, recentAvg, running, systemOverallHealth, total, useHistory, usualOf, useUsual, val, type DashSystem, type Usual } from "./data.ts";
import { SourceCard } from "./Rf.tsx";
import { SiteRows } from "./Systems.tsx";
import { BAD_MAX, badLevel, voiceBadPct } from "./Decode.tsx";

// ── today ────────────────────────────────────────────────────────────────────

function Today({ s }: { s: AppState }) {
  const systems = dashSystems(s);
  const h24 = useHistory(["all/calls", "all/audioBytes", "all/airMs", ...systems.flatMap((x) => [K.sys(x.name, "why/no_recorder"), K.sys(x.name, "why/no_source")])], "24h", 48);
  // Today (since local midnight) from the history: it spans restarts. This run's own count covers the minute not yet in it.
  const midnight = new Date();
  midnight.setHours(0, 0, 0, 0);
  const from = Math.floor(midnight.getTime() / 1000);
  const today = usePolled(`today:${from}`, () => statsSince(["all/calls", "all/airMs", "all/audioBytes"], from, 96), 60_000)?.series;
  const day = (k: string, session: string, mul = 1) => Math.max((total(today?.[k]) ?? 0) * mul, val(s, session) ?? 0);
  const h1 = useHistory(["all/calls", "all/audioBytes", "all/airMs"], "1h", 60);
  const usual = useUsual(["all/calls"]);
  // The last hour from the history; until it has a few minutes, what this page has seen.
  const perS = (k: string) => rateOver(h1?.[k]) ?? recentAvg(k, 600) ?? 0;
  const callsPerH = perS("all/calls") * 3600;
  const audioPerH = perS("all/audioBytes") * 3600;
  const onAir = perS("all/airMs") / 1000;
  const rec = val(s, "eng/recording") ?? 0;
  const recMax = val(s, "eng/recorders") ?? s.config?.recording.maxRecorders ?? 0;
  // Each system's, most first: with several, the stat says which.
  const missedBy = systems
    .map((x) => ({ name: x.name, n: Math.round((total(h24?.[K.sys(x.name, "why/no_recorder")]) ?? 0) + (total(h24?.[K.sys(x.name, "why/no_source")]) ?? 0)) }))
    .filter((m) => m.n > 0)
    .sort((a, b) => b.n - a.n);
  const missed = missedBy.reduce((a, m) => a + m.n, 0);
  const load = val(s, "eng/load");
  const perHour = (k: string) => (h24?.[k]?.v ?? []).map((v) => (v === null ? null : v * 3600));
  return (
    <div className="kpis">
      <Stat
        size="hero"
        label="Calls today"
        value={compact(Math.round(day("all/calls", "day/calls")))}
        trend={<Trend delta={change(callsPerH / 3600, usual["all/calls"])} title="Last hour vs usual" />}
        sub={<Figs inline items={[{ k: "Now", v: compact(callsPerH), u: "/h" }]} />}
        spark={<Sparkline v={perHour("all/calls")} title="Calls an hour, last 24 h" />}
      />
      <Stat
        label="Airtime today"
        value={hmm(day("all/airMs", "day/airS", 0.001))}
        sub={<Figs inline items={[{ k: "On air, avg", v: num(onAir, 1), title: "Calls on the air at once, average of the last hour" }]} />}
        spark={<Sparkline v={(h24?.["all/airMs"]?.v ?? []).map((v) => (v === null ? null : v / 1000))} color="var(--series-3)" title="Calls on the air at once, last 24 h" />}
      />
      <Stat label="Audio today" value={bytes(day("all/audioBytes", "day/audioBytes"))} sub={<Figs inline items={[{ k: "Now", v: bytes(audioPerH), u: "/h" }]} />} spark={<Sparkline v={perHour("all/audioBytes")} color="var(--series-6)" />} />
      <Stat
        label="Recorders"
        value={
          <>
            {rec}
            <small> / {recMax}</small>
          </>
        }
        level={recMax && rec >= recMax ? "warn" : undefined}
        spark={<Meter value={rec} max={Math.max(1, recMax)} zones={[recMax * 0.75, recMax]} label="recorders in use" />}
        sub={<Figs inline items={[{ k: "Channels open", v: val(s, "eng/channels") ?? 0 }]} />}
      />
      <Stat
        label="Missed, 24 h"
        value={compact(missed)}
        level={missed > 0 ? "warn" : undefined}
        sub={missed > 0 && systems.length > 1 ? <Figs inline items={missedBy.map((m) => ({ k: m.name, v: compact(m.n) }))} /> : undefined}
        hint="No free recorder, or out of every source's band"
        onClick={() => (missedBy.length === 1 ? setView("radio", missedBy[0].name) : setView("radio"))}
      />
      <Stat
        label="Decoding load"
        value={pct(load === null ? null : load * 100)}
        spark={<LiveSpark k="eng/load" mul={100} color="var(--series-2)" lo={0} />}
        hint="Of one CPU core: over 100% needs more than one"
      />
    </div>
  );
}

// ── a system ─────────────────────────────────────────────────────────────────

function SystemCard({ s, x, usuals, summary, freqs }: { s: AppState; x: DashSystem; usuals: Record<string, Usual | null>; summary?: RadioSummary; freqs?: FreqRow[] }) {
  const h = systemOverallHealth(s, x, usuals);
  // Its (first) site's control channel; with several, each in a row of its own too.
  const site = x.sites[0];
  const cc = (k: string) => (site ? K.cc(site, k) : K.sys(x.name, `cc/${k}`));
  const st = site?.status;
  const usual = usuals[cc("good")] ?? null;
  const several = x.sites.length > 1;
  const rate = several ? x.sites.reduce((a, y) => a + (val(s, K.cc(y, "good")) ?? 0), 0) : val(s, cc("good"));
  const good = recentAvg(cc("good"), 60) ?? 0;
  const bad = recentAvg(cc("bad"), 60) ?? 0;
  const decoded = good + bad > 0 ? (100 * good) / (good + bad) : null;
  const callsMin = (recentAvg(K.sys(x.name, "calls"), 600) ?? 0) * 60;
  const onAir = s.calls.filter((c) => c.systemName === x.name);
  const tgs = [...new Map(onAir.map((c) => [c.talkgroup, c])).values()].slice(0, 4);
  const lost = voiceBadPct(x.name);
  const snr = val(s, cc("snr"));
  // Not recording: the last day from the history instead.
  const day = useHistory([cc("good"), K.sys(x.name, "calls")], "24h", 48);
  const live = running(s);
  const dayRate = usualOf(day?.[cc("good")]);
  const dayCalls = total(day?.[K.sys(x.name, "calls")]);
  // A conventional system: its calls this run, and its channels (open: a signal on them now).
  const channels = x.kind === "conventional" ? (s.config?.conventional.find((v) => v.shortName === x.name)?.channels.filter((ch) => ch.enabled).length ?? 0) : 0;
  return (
    <Card
      title={
        <SystemTag config={s.config} name={x.name} />
      }
      level={h.level}
      why={h.why}
      onWhy={goTo(h)}
      className="sys-card"
      onTitle={() => setView("decode", x.name)}
      actions={<span className="badge-kind">{st?.modulation ? `${KIND_LABEL[x.kind]} · ${st.modulation}` : KIND_LABEL[x.kind]}</span>}
    >
      {!live ? (
        <Stat
          label="Last 24 h"
          value={dayCalls === null ? "—" : compact(dayCalls)}
          unit="calls"
          spark={<Sparkline v={day?.[cc("good")]?.v ?? []} title="Control msg/s, last 24 h" />}
          sub={dayRate ? <Figs inline items={[{ k: "Control avg", v: num(dayRate.avg), u: "msg/s" }]} /> : "No history yet"}
        />
      ) : x.kind !== "conventional" && several ? (
        <div className="sys-main">
          <Stat label="Control channels" value={num(rate)} unit="msg/s" sub={<Fig v={x.sites.length} u="sites" />} />
          <SiteRows s={s} x={x} />
        </div>
      ) : x.kind !== "conventional" ? (
        <div className="sys-main">
          <Stat
            label="Control channel"
            value={num(rate)}
            unit="msg/s"
            trend={<Trend delta={change(recentAvg(cc("good"), 60), usual)} title="vs usual" />}
            spark={<LiveSpark k={cc("good")} band={usual ? [usual.lo, usual.hi] : null} lo={0} title="msg/s, 10 min; band is usual" />}
            sub={
              <Figs
                inline
                items={[
                  { k: "Decoded", v: decoded === null ? "…" : num(decoded, 0), u: decoded === null ? undefined : "%", level: decoded === null ? undefined : decoded < 70 ? "bad" : decoded < 90 ? "warn" : undefined },
                  st?.controlChannelHz ? { k: "CC", v: formatMhz(st.controlChannelHz), u: "MHz", title: "Control channel" } : { k: "CC", v: "hunting", level: "warn" },
                  snr !== null && { k: "SNR", v: num(snr, 0), u: "dB", level: snr < 8 ? "bad" : snr < 15 ? "warn" : undefined },
                ]}
              />
            }
          />
        </div>
      ) : (
        <div className="sys-main">
          <Stat
            label="Calls since start"
            value={x.status ? compact(x.status.callsConcluded) : "—"}
            spark={<LiveSpark k={K.sys(x.name, "calls")} mul={60} lo={0} title="Calls a minute, 10 min" />}
            sub={<Figs inline items={[{ k: "24 h", v: dayCalls === null ? "—" : compact(dayCalls) }, { k: "Channels", v: channels }, !!x.status?.conventionalOpen && { k: "Open", v: x.status.conventionalOpen, title: "A signal on them now" }]} />}
          />
        </div>
      )}
      {live && <div className="mini-stats">
        <div>
          <b>{num(callsMin, 1)}</b>
          <span>calls/min</span>
        </div>
        <div>
          <b>{onAir.length}</b>
          <span>on the air</span>
        </div>
        <div title="Unusable voice frames, 10 min (under 2% is clean)">
          <b className={badLevel(lost) === "ok" ? "" : badLevel(lost)}>{lost === null ? "—" : `${num(lost, 1)}%`}</b>
          <span>voice lost</span>
        </div>
        <div title="First heard in the last 24 h">
          <b className={summary?.newTgs.length ? "accent" : ""}>{summary ? summary.newTgs.length : "—"}</b>
          <span>new TGs</span>
        </div>
      </div>}
      {tgs.length > 0 && (
        <div className="row tg-chips">
          {tgs.map((c) => (
            <button key={c.talkgroup} className="chip" onClick={() => setView("radio", x.name, "tg", c.talkgroup)} title={`TG ${c.talkgroup}`}>
              <span className={`dot dot-${c.state}`} />
              {c.alphaTag || c.talkgroup}
            </button>
          ))}
        </div>
      )}
      {freqs && freqs.length > 0 && (
        <div className="freq-strip">
          <HeatStrip
            v={freqs.map((f) => f.badPct)}
            ramp="hot"
            max={BAD_MAX}
            titles={freqs.map((f) => `${mhz(f.freqHz)} MHz · ${f.calls} calls · ${f.badPct ?? "—"}% voice lost${f.snr !== null ? ` · SNR ${num(f.snr)} dB` : ""}`)}
            onCell={() => setView("decode", x.name)}
            label="voice lost per frequency"
          />
          <span className="muted small">Voice lost per frequency · 24 h</span>
        </div>
      )}
    </Card>
  );
}

// ── events ───────────────────────────────────────────────────────────────────

export function eventText(e: MonitorEvent): string {
  switch (e.kind) {
    case "tgFirstSeen":
      return `${e.system}: new talkgroup ${e.alphaTag || e.talkgroup}`;
    case "unitFirstSeen":
      return `${e.system}: new radio ${e.alias || e.unit}`;
    case "tonePattern":
      return `${e.system}: ${e.signals?.length ? `${e.signals.join(", ")} (${e.text})` : e.text} on talkgroup ${e.alphaTag || e.talkgroup}`;
    case "controlLost":
      return `${e.system}${e.site ? ` site ${e.site}` : ""}: control channel lost${e.freqHz ? ` · ${mhz(e.freqHz)} MHz` : ""}`;
    case "controlRegained":
      return `${e.system}${e.site ? ` site ${e.site}` : ""}: control channel back`;
    case "sourceDrops":
      return `${e.source}: dropping samples`;
    case "sourceClipping":
      return `${e.source}: clipping ${e.pct}%`;
    case "sourceRecovered":
      return `${e.source}: clean again`;
    case "pluginHealth":
      return `${e.plugin}: ${e.state}${e.message ? ` — ${e.message}` : ""}`;
    case "linkDown":
      return `${e.target}: not answering`;
    case "linkUp":
      return `${e.target}: back · down ${e.downS ? `${Math.round(e.downS)} s` : "briefly"}`;
    case "diskLow":
      return `Disk ${e.path}: ${e.freePct}% free`;
    case "spoolLow":
      return `RAM spool ${e.freePct}% free — ${e.critical ? "calls soon go to disk" : "uploads behind"}`;
    case "spoolFull":
      return `RAM spool full: ${e.calls} call${e.calls === 1 ? "" : "s"} to the recordings folder`;
    case "spoolRecovered":
      return `RAM spool OK · ${e.freePct}% free`;
    case "spoolRemoved":
      return `RAM spool deleted${e.remade ? " · made again" : " — calls go to the recordings folder"}`;
    case "clockOff":
      return e.why ?? "The clock is off";
    case "clockOk":
      return "Clock right again";
    case "clockStep":
      return (e.byMs ?? 0) > 0 ? `Clock jumped ${ms(e.byMs)} forward` : `Clock set back ${ms(e.byMs)}`;
    default:
      return `${e.kind}${e.system ? ` (${e.system})` : ""}`;
  }
}

/** The longer why behind an event, if any: its row's tooltip. */
function eventTip(e: MonitorEvent): string | undefined {
  switch (e.kind) {
    case "spoolRemoved":
      return spoolRemovedFix(e.spool);
    case "clockStep":
      return (e.byMs ?? 0) > 0 ? "Its time service set it, or the computer slept" : undefined;
    case "sourceClipping":
      return "Samples at full scale";
    default:
      return undefined;
  }
}

function eventPlace(e: MonitorEvent): () => void {
  switch (e.kind) {
    case "tgFirstSeen":
      return () => setView("radio", e.system ?? "", "tg", e.talkgroup ?? 0);
    case "unitFirstSeen":
      return () => setView("radio", e.system ?? "", "unit", e.unit ?? 0);
    case "tonePattern":
      return () => setView("radio", e.system ?? "");
    case "controlLost":
    case "controlRegained":
      return () => setView("decode", e.system ?? "");
    case "sourceDrops":
    case "sourceClipping":
    case "sourceRecovered":
      return () => setView("rf");
    case "pluginHealth":
      return () => setView("plugins");
    default:
      return () => setView("platform");
  }
}

export function EventFeed({ events, limit = 12 }: { events: MonitorEvent[]; limit?: number }) {
  const shown = [...events].reverse().slice(0, limit);
  return (
    <Card title="Lately" className="events">
      {shown.length === 0 ? (
        <p className="empty">Nothing notable yet.</p>
      ) : (
        <ul className="feed">
          {shown.map((e, i) => (
            <li key={`${e.t}-${i}`}>
              <button className="linkish feed-item" onClick={eventPlace(e)} title={eventTip(e)}>
                <Light level={e.level === "info" ? "idle" : e.level} />
                <span className="feed-text">{eventText(e)}</span>
                <span className="muted small">{ago(e.t)}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

// ── plugins and the computer in a line ───────────────────────────────────────

function PluginsLine({ s }: { s: AppState }) {
  const on = (s.plugins?.plugins ?? []).filter((p) => pluginOn(s.config, p.id));
  return (
    <Card title="Plugins" onTitle={() => setView("plugins")}>
      {on.length === 0 ? (
        <p className="empty">No plugins on.</p>
      ) : (
        <ul className="line-list">
          {on.map((p) => {
            const h = pluginHealth(p, true);
            const m = p.runtime.metrics;
            return (
              <li key={p.id}>
                <Light level={h.level} title={h.why} />
                <b>{p.manifest?.name ?? p.id}</b>
                <Figs inline items={[{ k: "Sent", v: compact(p.runtime.ok) }, { k: "Failed", v: compact(p.runtime.failed), level: p.runtime.failed ? "warn" : undefined }, !!m?.queued && { k: "Queued", v: compact(m.queued) }]} />
              </li>
            );
          })}
        </ul>
      )}
    </Card>
  );
}

function PlatformLine({ s }: { s: AppState }) {
  const h = platformHealth(s);
  const v = s.host?.values ?? {};
  const disk = s.host?.platform.disks.find((d) => d.name === "recordings") ?? s.host?.platform.disks[0];
  const spool = s.host?.platform.disks.find((d) => d.name === "spool");
  const net = connectionsSummary(connections(s));
  return (
    <Card title="Computer" level={s.host ? h.level : undefined} why={h.why} onTitle={() => setView("platform")}>
      <div className="meters">
        <label>
          <span>CPU</span>
          <Meter value={v["plat/cpu"] ?? null} max={100} zones={[75, 90]} label="CPU" />
          <b>{pct(v["plat/cpu"])}</b>
        </label>
        <label>
          <span>Memory</span>
          <Meter value={v["plat/mem"] ?? null} max={100} zones={[85, 95]} label="memory" />
          <b>{pct(v["plat/mem"])}</b>
        </label>
        {disk && (
          <label>
            <span>Disk</span>
            <Meter value={100 - (100 * disk.freeBytes) / Math.max(1, disk.totalBytes)} max={100} zones={[90, 95]} label="disk" />
            <b>{bytes(disk.freeBytes)} free</b>
          </label>
        )}
        {spool && (
          <label>
            <span>RAM spool</span>
            <Meter value={spool.missing ? null : 100 - (100 * spool.freeBytes) / Math.max(1, spool.totalBytes)} max={100} zones={[75, 90]} label="RAM spool" />
            <b>{spool.missing ? "gone" : `${bytes(spool.freeBytes)} free`}</b>
          </label>
        )}
        {net && (
          <label title={net.why}>
            <span>Connections</span>
            <Light level={net.level} />
            <b>{net.value === "Up" && v["net/rtt"] !== undefined ? `${num(v["net/rtt"], 0)} ms` : net.value.toLowerCase()}</b>
          </label>
        )}
      </div>
    </Card>
  );
}

// ── the page ─────────────────────────────────────────────────────────────────

export function Overview() {
  const s = useApp();
  const on = running(s);
  const systems = dashSystems(s);
  const usual = useUsual(systems.flatMap((x) => x.sites.map((y) => K.cc(y, "good"))));
  const summary = usePolled(`summary:${on}`, () => radioQuery({ what: "summary" }), on ? 60_000 : 0);
  const names = systems.map((x) => x.name).join(",");
  const freqs = usePolled(`freqs:${names}:${on}`, () => Promise.all(systems.map((x) => radioQuery({ what: "freqs", system: x.name, hours: 24 }))), on ? 120_000 : 0);
  const freqOf = (name: string) => freqs?.find((f) => f.system === name)?.rows as FreqRow[] | undefined;
  return (
    <div className="page">
      {on ? (
        <Today s={s} />
      ) : (
        <section className="stopped">
          <div>
            <h2 className="big">Not recording</h2>
            <p className="muted">
              {s.config && systems.length ? `${systems.length} system${systems.length > 1 ? "s" : ""} set up` : "Set up a source and a system, then Start"}
            </p>
          </div>
          {mayEdit() && (
            <div className="row">
              <button className="btn primary" onClick={start} disabled={!s.connected}>
                Start
              </button>
              <button className="btn ghost" onClick={() => setView("setup")}>
                Setup
              </button>
            </div>
          )}
        </section>
      )}
      <div className="overview-grid">
        <div className="stack">
          <h2 className="section-title">Systems</h2>
          {systems.length === 0 && <p className="empty">No systems set up yet.</p>}
          <div className="card-grid">
            {systems.map((x) => (
              <SystemCard key={x.name} s={s} x={x} usuals={usual} summary={summary?.systems?.[x.name]} freqs={freqOf(x.name)} />
            ))}
          </div>
        </div>
        <div className="stack">
          {on && s.sources.length > 0 && (
            <>
              <h2 className="section-title">Sources</h2>
              {s.sources.map((src, i) => (
                <SourceCard key={i} s={s} src={src} i={i} compact />
              ))}
            </>
          )}
          <h2 className="section-title">Status</h2>
          {can("host") && <PlatformLine s={s} />}
          {can("plugins") && mayEdit() && <PluginsLine s={s} />}
          <EventFeed events={s.events} />
        </div>
      </div>
    </div>
  );
}
