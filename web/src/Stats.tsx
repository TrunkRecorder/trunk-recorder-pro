// The Stats page (desktop app): what each system's recorded calls and
// control channel have been doing, and which radios met which talkgroups —
// for one site, or all the sites of a multi-site system together.

import { useEffect, useMemo, useState } from "react";
import { formatMhz, siteSiblings } from "./config.ts";
import { clearStatsHistory, fetchAffiliationLinks, fetchAffiliations, fetchStats, fetchStatsHistory, useApp, type AppState } from "./controller.ts";
import { TimeChart, type ChartSeries } from "./charts.tsx";
import type { AffiliationLink, AffiliationRow, AffiliationView, HistoryKind, StatRow, StatsTable, StatsTarget, StatsWindow } from "./protocol.ts";
import { Tile } from "./Tile.tsx";

const WINDOWS: { id: StatsWindow; label: string }[] = [
  { id: "restart", label: "Since restart" },
  { id: "24h", label: "24 hours" },
  { id: "7d", label: "7 days" },
  { id: "30d", label: "30 days" },
  { id: "all", label: "All" },
];

/** Bit error rate, %. */
const ber = (r: { errors: number; codedBits: number }) => (r.codedBits > 0 ? (100 * r.errors) / r.codedBits : null);
const pct = (v: number | null, digits = 2) => (v === null ? "—" : `${v.toFixed(digits)}%`);
/** Under 1% sounds clean, 1–2% is audible, over 2% is degraded. */
const berTone = (v: number | null): "ok" | "warn" | "bad" | undefined => (v === null ? undefined : v < 1 ? "ok" : v <= 2 ? "warn" : "bad");

function duration(s: number): string {
  if (s < 60) return `${Math.round(s)} s`;
  const m = Math.round(s / 60);
  return m < 60 ? `${m} min` : `${Math.floor(m / 60)} h ${m % 60} min`;
}

function ago(t: number): string {
  const s = Date.now() / 1000 - t;
  if (s < 90) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 48 * 3600) return `${Math.round(s / 3600)} h ago`;
  return `${Math.round(s / 86400)} days ago`;
}

const when = (t: number) => new Date(t * 1000).toLocaleString();

/** The systems statistics are kept for: the trunked ones, then the conventional ones. */
function systemNames(s: AppState): string[] {
  const c = s.config;
  if (!c) return [];
  return [...c.systems.map((x) => x.shortName), ...c.conventional.filter((x) => x.channels.length > 0 || x.channelFile).map((x) => x.shortName)];
}

const GROUPS_KEY = "trp.siteGroups";

/** Multi-site systems as last seen recording, kept in this browser for when it isn't. */
function rememberedGroups(): string[][] {
  try {
    const v = JSON.parse(localStorage.getItem(GROUPS_KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((g) => Array.isArray(g) && g.every((x) => typeof x === "string")) : [];
  } catch {
    return [];
  }
}

/**
 * What the System menu offers: each system, then each multi-site system's
 * sites together. Sites belong together as the recorder reports them (site
 * group, or the same WACN and System ID), or as the config groups them.
 */
function statsTargets(s: AppState): StatsTarget[] {
  const names = systemNames(s);
  const out: StatsTarget[] = names.map((n) => ({ key: n, label: n, systems: [n] }));
  const groups = new Map<string, Set<string>>();
  const add = (key: string, name: string) => groups.set(key, (groups.get(key) ?? new Set()).add(name));
  for (const x of s.status?.systems ?? []) {
    const key = x.siteGroup ?? (x.identity.wacn != null && x.identity.sysId != null ? `${x.identity.wacn}/${x.identity.sysId}` : null);
    if (key) add(key, x.shortName);
  }
  for (const x of s.config?.systems ?? []) {
    const sibs = s.config ? siteSiblings(s.config, x) : [];
    if (sibs.length)
      [x, ...sibs].forEach((y) =>
        add(
          `config:${[x, ...sibs]
            .map((z) => z.shortName)
            .sort()
            .join(",")}`,
          y.shortName,
        ),
      );
  }
  const live = [...groups.values()].map((g) => [...g].sort()).filter((g) => g.length > 1);
  if (live.length && s.status) {
    try {
      localStorage.setItem(GROUPS_KEY, JSON.stringify(live));
    } catch {
      // Not remembered.
    }
  }
  const seen = new Set<string>();
  for (const g of [...live, ...rememberedGroups()]) {
    const members = g.filter((n) => names.includes(n));
    const key = `sites:${members.join(",")}`;
    if (members.length < 2 || seen.has(key)) continue;
    seen.add(key);
    out.push({ key, label: `${members.join(" + ")} (all sites)`, systems: members });
  }
  return out;
}

export function StatsPage() {
  const s = useApp();
  const targets = statsTargets(s);
  const [choice, setChoice] = useState<string>("");
  const [window, setWindow] = useState<StatsWindow>("24h");
  const [tab, setTab] = useState<"overview" | "affiliations">("overview");
  const target = targets.find((x) => x.key === choice) ?? targets[0] ?? null;
  const key = target?.key ?? "";

  useEffect(() => {
    if (!target || !s.connected || tab !== "overview") return;
    fetchStats(target, window);
    // Recent windows keep up while the page is open.
    if (window !== "restart" && window !== "24h") return;
    const t = setInterval(() => document.visibilityState === "visible" && fetchStats(target, window), 30000);
    return () => clearInterval(t);
    // (The target is the same while its key is.)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, window, s.connected, tab]);

  if (!s.config) return <p className="muted">Connecting to the recorder…</p>;
  if (!target) return <p className="muted">No systems are set up yet.</p>;

  return (
    <div className="stats stack">
      <div className="stats-bar">
        <label className="field">
          <span className="field-label">System</span>
          <select value={target.key} onChange={(e) => setChoice(e.target.value)}>
            {targets.map((x) => (
              <option key={x.key} value={x.key}>
                {x.label}
              </option>
            ))}
          </select>
        </label>
        <nav className="tabs" aria-label="Statistics">
          <button className={tab === "overview" ? "on" : ""} onClick={() => setTab("overview")}>
            Overview
          </button>
          <button className={tab === "affiliations" ? "on" : ""} onClick={() => setTab("affiliations")}>
            Affiliations
          </button>
        </nav>
        {tab === "overview" && (
          <div className="segmented" role="radiogroup" aria-label="Over">
            {WINDOWS.map((w) => (
              <button key={w.id} role="radio" aria-checked={window === w.id} className={window === w.id ? "on" : ""} onClick={() => setWindow(w.id)}>
                {w.label}
              </button>
            ))}
          </div>
        )}
      </div>
      {tab === "overview" ? <Overview s={s} target={target} window={window} /> : <Affiliations s={s} target={target} />}
    </div>
  );
}

function Overview({ s, target, window }: { s: AppState; target: StatsTarget; window: StatsWindow }) {
  const sys = target.key;
  const st = s.stats && s.stats.system === sys && s.stats.window === window ? s.stats : null;
  const [detail, setDetail] = useState<{ kind: HistoryKind; id: number; label: string } | null>(null);
  const [errorsBy, setErrorsBy] = useState<"talkgroups" | "units">("talkgroups");
  useEffect(() => {
    setDetail(null);
    clearStatsHistory();
  }, [sys]);
  useEffect(() => {
    if (detail) fetchStatsHistory(target, detail.kind, detail.id, window);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [detail, window, sys]);
  const open = (kind: HistoryKind, id: number, label: string) => setDetail(detail?.kind === kind && detail.id === id ? null : { kind, id, label });

  if (!st) return <p className="muted">Loading…</p>;
  const t = st.totals;
  const b = ber(t);
  const heard = Math.max(t.grants, t.calls);
  return (
    <>
      <div className="tiles">
        <Tile label="Calls recorded" value={t.calls.toLocaleString()} sub={t.grants ? `of ${t.grants.toLocaleString()} heard` : undefined} />
        <Tile label="Audio" value={duration(t.seconds)} sub={t.calls ? `${(t.seconds / t.calls).toFixed(1)} s a call` : undefined} />
        <Tile
          label="Bit error rate"
          value={pct(b)}
          tone={berTone(b)}
          sub={t.badFrames ? `${pct(t.frames ? (100 * t.badFrames) / t.frames : null, 1)} frames repeated or muted` : "of the voice received"}
        />
        <Tile label="Encrypted" value={heard ? pct((100 * t.encrypted) / heard, 0) : "—"} sub={`${t.notRecorded.toLocaleString()} not recorded`} />
        <Tile label="Emergency" value={t.emergency.toLocaleString()} sub="calls flagged" tone={t.emergency ? "warn" : undefined} />
      </div>
      <div className="columns">
        <section className="panel">
          <header className="panel-head">
            <h2>Control channel</h2>
            <span className="muted small">{st.rates.bucket === 60 ? "per minute" : "per hour"}</span>
          </header>
          <RatesChart rates={st.rates} />
        </section>
        <section className="panel">
          <header className="panel-head">
            <h2>Calls</h2>
            <span className="muted small">per hour</span>
          </header>
          <HoursChart hours={st.hours} />
        </section>
      </div>
      {detail && (
        <section className="panel">
          <header className="panel-head">
            <h2>{detail.label}</h2>
            <button className="btn ghost small" onClick={() => setDetail(null)}>
              Close
            </button>
          </header>
          {s.statsHistory && s.statsHistory.kind === detail.kind && s.statsHistory.id === detail.id ? (
            <HoursChart hours={s.statsHistory.hours} />
          ) : (
            <p className="muted">Loading…</p>
          )}
        </section>
      )}
      <div className="columns">
        <section className="panel">
          <header className="panel-head">
            <h2>Channels</h2>
            <span className="muted small">Errors on one channel point at the site's setup or interference</span>
          </header>
          <StatTable
            rows={st.channels}
            first="Frequency"
            name={(r) => <span className="mono">{formatMhz(r.freq!)}</span>}
            onPick={(r) => open("freq", r.freq!, `Channel ${formatMhz(r.freq!)}`)}
          />
        </section>
        <section className="panel">
          <header className="panel-head">
            <h2>Busiest talkgroups</h2>
          </header>
          <StatTable
            rows={st.talkgroups}
            first="Talkgroup"
            name={(r) => <Named id={r.talkgroup!} alias={r.alias} />}
            grants
            onPick={(r) => open("talkgroup", r.talkgroup!, `Talkgroup ${r.alias || r.talkgroup}`)}
          />
        </section>
      </div>
      <section className="panel">
        <header className="panel-head">
          <h2>Most decoder errors</h2>
          <div className="segmented small">
            <button className={errorsBy === "talkgroups" ? "on" : ""} onClick={() => setErrorsBy("talkgroups")}>
              Talkgroups
            </button>
            <button className={errorsBy === "units" ? "on" : ""} onClick={() => setErrorsBy("units")}>
              Radios
            </button>
          </div>
        </header>
        <p className="muted small">
          Errors that follow a talkgroup or radio wherever it's heard were in the signal as received: portables in buildings, radios on the move.
        </p>
        {errorsBy === "talkgroups" ? (
          <StatTable
            rows={st.topErrors.talkgroups}
            first="Talkgroup"
            name={(r) => <Named id={r.talkgroup!} alias={r.alias} />}
            onPick={(r) => open("talkgroup", r.talkgroup!, `Talkgroup ${r.alias || r.talkgroup}`)}
          />
        ) : (
          <StatTable
            rows={st.topErrors.units}
            first="Radio"
            name={(r) => <Named id={r.unit!} alias={r.alias} />}
            onPick={(r) => open("unit", r.unit!, `Radio ${r.alias || r.unit}`)}
          />
        )}
      </section>
      {st.dropped > 0 && (
        <p className="muted small">The statistics writer fell behind and missed {st.dropped.toLocaleString()} events since the recorder started.</p>
      )}
    </>
  );
}

function Named({ id, alias }: { id: number; alias: string }) {
  return alias ? (
    <span title={String(id)}>
      {alias} <span className="muted mono small">{id}</span>
    </span>
  ) : (
    <span className="mono">{id}</span>
  );
}

function StatTable(props: { rows: StatRow[]; first: string; name: (r: StatRow) => React.ReactNode; grants?: boolean; onPick?: (r: StatRow) => void }) {
  if (props.rows.length === 0) return <p className="muted">Nothing yet.</p>;
  const most = Math.max(...props.rows.map((r) => r.seconds), 1);
  return (
    <div className="table-scroll">
      <table className="stat-table">
        <thead>
          <tr>
            <th>{props.first}</th>
            <th className="num">{props.grants ? "Heard" : "Calls"}</th>
            <th>Audio</th>
            <th className="num">Bit errors</th>
            <th className="num">Bad frames</th>
          </tr>
        </thead>
        <tbody>
          {props.rows.map((r, i) => {
            const b = ber(r);
            return (
              <tr key={i} className={props.onPick ? "pick" : ""} onClick={() => props.onPick?.(r)}>
                <td>{props.name(r)}</td>
                <td className="num">{(props.grants ? Math.max(r.grants, r.calls) : r.calls).toLocaleString()}</td>
                <td>
                  <div className="bar-cell">
                    <span className="bar" style={{ width: `${(100 * r.seconds) / most}%` }} />
                    <span>{duration(r.seconds)}</span>
                  </div>
                </td>
                <td className={`num tone-text-${berTone(b) ?? "none"}`}>{pct(b)}</td>
                <td className="num">{pct(r.frames ? (100 * r.badFrames) / r.frames : null, 1)}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function column(t: StatsTable, name: string): number[] {
  const i = t.columns.indexOf(name);
  return t.rows.map((r) => r[i]);
}

const RATE_SERIES: ChartSeries[] = [
  { label: "Messages/s", color: "--sys-0", format: (v) => v.toFixed(1) },
  { label: "Active calls", color: "--sys-1", right: true, stepped: true, format: (v) => v.toFixed(0) },
];

function RatesChart({ rates }: { rates: StatsTable & { bucket: number } }) {
  const data = useMemo(() => ({ x: column(rates, "t"), ys: [column(rates, "decode"), column(rates, "active")] }), [rates]);
  if (data.x.length === 0) return <p className="muted">Nothing yet.</p>;
  return <TimeChart x={data.x} ys={data.ys} series={RATE_SERIES} leftLabel="messages/s" rightLabel="calls" />;
}

const HOUR_SERIES: ChartSeries[] = [
  { label: "Calls", color: "--sys-0", bars: true, format: (v) => v.toFixed(0) },
  { label: "Bit errors", color: "--bad", right: true, format: (v) => `${v.toFixed(2)}%` },
];

function HoursChart({ hours }: { hours: StatsTable }) {
  const data = useMemo(() => {
    const calls = column(hours, "calls");
    const errors = column(hours, "errors");
    const bits = column(hours, "codedBits");
    return { x: column(hours, "hour"), ys: [calls, errors.map((e, i) => (bits[i] > 0 ? (100 * e) / bits[i] : null))] };
  }, [hours]);
  if (data.x.length === 0) return <p className="muted">Nothing yet.</p>;
  return <TimeChart x={data.x} ys={data.ys} series={HOUR_SERIES} leftLabel="calls" rightLabel="bit errors" pad={1800} />;
}

/** On, or idle for 6 or 12 hours, or off: how a radio is doing, from what was last heard. */
function radioStatus(registered: boolean | null | undefined, lastSeen: number): { label: string; tone: "ok" | "warn" | "off" } {
  if (registered === false) return { label: "Off", tone: "off" };
  const hours = (Date.now() / 1000 - lastSeen) / 3600;
  if (hours < 6) return { label: "Active", tone: "ok" };
  return { label: hours < 12 ? "Idle 6 h+" : "Idle 12 h+", tone: "warn" };
}

/** Where the Affiliations list is: a view and a search, or (after a jump) one radio or talkgroup. */
interface Place {
  view: AffiliationView;
  search: string;
  focus: number | null;
}

function Affiliations({ s, target }: { s: AppState; target: StatsTarget }) {
  const sys = target.key;
  const [place, setPlace] = useState<Place>({ view: "units", search: "", focus: null });
  // Where Back goes: the places before each jump.
  const [back, setBack] = useState<Place[]>([]);
  const [open, setOpen] = useState<number | null>(null);
  const { view, search, focus } = place;
  const units = view === "units";
  const multi = target.systems.length > 1;

  // Another system: start over.
  useEffect(() => {
    setPlace((p) => ({ view: p.view, search: "", focus: null }));
    setBack([]);
  }, [sys]);
  // Searched a moment after typing stops; a jump opens what it jumped to.
  useEffect(() => {
    const t = setTimeout(() => s.connected && fetchAffiliations(target, view, search, 0, focus), search ? 300 : 0);
    setOpen(focus);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sys, view, search, focus, s.connected]);
  useEffect(() => {
    if (open !== null) fetchAffiliationLinks(target, view, open);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, sys, view]);

  const go = (p: Place) => {
    setBack((b) => [...b, place].slice(-50));
    setPlace(p);
  };
  // A talkgroup under a radio (or a radio under a talkgroup): over to its side, opened.
  const jump = (id: number, to: AffiliationView = units ? "talkgroups" : "units") => go({ view: to, search: "", focus: id });
  const goBack = () => {
    const p = back[back.length - 1];
    if (!p) return;
    setBack(back.slice(0, -1));
    setPlace(p);
  };

  const a =
    s.affiliations && s.affiliations.system === sys && s.affiliations.view === view && s.affiliations.search === search && s.affiliations.id === focus
      ? s.affiliations
      : null;
  const links =
    s.affiliationLinks && s.affiliationLinks.system === sys && s.affiliationLinks.view === view && s.affiliationLinks.id === open ? s.affiliationLinks : null;
  const exportUrl = `/api/stats/export?${target.systems.map((x) => `system=${encodeURIComponent(x)}`).join("&")}`;
  const focused = focus !== null ? a?.rows[0] : undefined;
  return (
    <section className="panel">
      <header className="panel-head">
        <div className="segmented">
          <button className={units ? "on" : ""} onClick={() => !units && go({ view: "units", search: "", focus: null })}>
            Radios
          </button>
          <button className={!units ? "on" : ""} onClick={() => units && go({ view: "talkgroups", search: "", focus: null })}>
            Talkgroups
          </button>
        </div>
        <input
          type="search"
          className="search"
          placeholder={units ? "Radio ID or name" : "Talkgroup or name"}
          value={search}
          onChange={(e) => setPlace({ view, search: e.target.value, focus: null })}
        />
        <span className="muted small">{a && focus === null ? `${a.total.toLocaleString()} ${units ? "radios" : "talkgroups"}` : ""}</span>
        <a className="btn ghost small" href={exportUrl} download>
          Export JSON
        </a>
      </header>
      {(back.length > 0 || focus !== null) && (
        <div className="aff-trail">
          {back.length > 0 && (
            <button className="btn ghost small" onClick={goBack}>
              ← Back
            </button>
          )}
          {focus !== null && (
            <>
              <span>
                {units ? "Radio" : "Talkgroup"} <Named id={focus} alias={focused?.alias ?? ""} />
              </span>
              <button className="btn ghost small" onClick={() => go({ view, search: "", focus: null })}>
                Show all {units ? "radios" : "talkgroups"}
              </button>
            </>
          )}
        </div>
      )}
      <p className="muted small">
        A radio and a talkgroup are linked whenever they meet: on a call, when the radio joins the talkgroup, or when it reports its location for it. Click a
        row to see {units ? "the talkgroups a radio has met" : "the radios a talkgroup has met"}, and click one of those to go to it.
        {multi ? ` Counts are for all of ${target.systems.join(", ")} together.` : ""}
      </p>
      {!a ? (
        <p className="muted">Loading…</p>
      ) : a.rows.length === 0 ? (
        <p className="muted">{focus !== null ? "Not heard here." : search ? "Nothing matches." : "Nothing heard yet."}</p>
      ) : (
        <div className="table-scroll">
          <table className="stat-table">
            <thead>
              <tr>
                <th>{units ? "Radio" : "Talkgroup"}</th>
                {units && <th>Status</th>}
                <th>Last heard</th>
                <th>First heard</th>
                <th className="num">Calls</th>
                <th className="num">Joined</th>
                <th className="num">{units ? "Talkgroups" : "Radios"}</th>
                {units && <th>Last talkgroup</th>}
                {multi && <th>Sites</th>}
              </tr>
            </thead>
            <tbody>
              {a.rows.map((r) => {
                const id = (units ? r.unit : r.talkgroup)!;
                return (
                  <AffiliationRows
                    key={id}
                    row={r}
                    id={id}
                    units={units}
                    allSites={multi ? target.systems : null}
                    open={open === id}
                    onToggle={() => setOpen(open === id ? null : id)}
                    links={open === id ? (links?.rows ?? null) : null}
                    onJump={jump}
                  />
                );
              })}
            </tbody>
          </table>
          {focus === null && a.rows.length < a.total && (
            <button className="btn ghost" onClick={() => fetchAffiliations(target, view, search, a.rows.length)}>
              Show {Math.min(200, a.total - a.rows.length)} more
            </button>
          )}
        </div>
      )}
    </section>
  );
}

/** The sites something was heard on: all of them, or which. */
function sitesText(sites: string[], all: string[]): string {
  return sites.length >= all.length ? "all sites" : sites.join(", ");
}

function AffiliationRows(props: {
  row: AffiliationRow;
  id: number;
  units: boolean;
  /** A multi-site system's sites (null: one site). */
  allSites: string[] | null;
  open: boolean;
  onToggle: () => void;
  links: AffiliationLink[] | null;
  /** Go to a radio or talkgroup (`to`: its side; the other side by default). */
  onJump: (id: number, to?: AffiliationView) => void;
}) {
  const { row: r, units, allSites } = props;
  const status = units ? radioStatus(r.registered, r.lastSeen) : null;
  const columns = (units ? 8 : 6) + (allSites ? 1 : 0);
  return (
    <>
      <tr className="pick" onClick={props.onToggle} aria-expanded={props.open}>
        <td>
          <Named id={props.id} alias={r.alias} />
        </td>
        {status && (
          <td>
            <span className={`status-pill status-${status.tone}`}>{status.label}</span>
          </td>
        )}
        <td title={when(r.lastSeen)}>{ago(r.lastSeen)}</td>
        <td title={when(r.firstSeen)}>{new Date(r.firstSeen * 1000).toLocaleDateString()}</td>
        <td className="num">{r.calls.toLocaleString()}</td>
        <td className="num">{r.affiliations.toLocaleString()}</td>
        <td className="num">{((units ? r.talkgroups : r.units) ?? 0).toLocaleString()}</td>
        {units && (
          <td>
            {r.lastTalkgroup ? (
              <button
                className="link-button mono"
                title="Go to this talkgroup"
                onClick={(e) => {
                  e.stopPropagation();
                  props.onJump(r.lastTalkgroup!, "talkgroups");
                }}
              >
                {r.lastTalkgroup}
              </button>
            ) : (
              "—"
            )}
          </td>
        )}
        {allSites && <td className="muted small">{sitesText(r.sites, allSites)}</td>}
      </tr>
      {props.open && (
        <tr className="links-row">
          <td colSpan={columns}>
            {!props.links ? (
              <span className="muted">Loading…</span>
            ) : props.links.length === 0 ? (
              <span className="muted">Not heard with any {units ? "talkgroup" : "radio"} yet.</span>
            ) : (
              <div className="links">
                {props.links.map((l) => (
                  <button
                    key={l.id}
                    className="link-chip"
                    title={`Go to this ${units ? "talkgroup" : "radio"} · ${when(l.firstSeen)} – ${when(l.lastSeen)}`}
                    onClick={() => props.onJump(l.id)}
                  >
                    <Named id={l.id} alias={l.alias} />{" "}
                    <span className="muted small">
                      {[
                        l.voice && `${l.voice} call${l.voice === 1 ? "" : "s"}`,
                        l.affiliations && `${l.affiliations} joined`,
                        l.locations && `${l.locations} location`,
                      ]
                        .filter(Boolean)
                        .join(" · ")}
                      {allSites ? ` · ${sitesText(l.sites, allSites)}` : ""}
                    </span>
                  </button>
                ))}
              </div>
            )}
          </td>
        </tr>
      )}
    </>
  );
}
