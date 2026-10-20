// Identifying systems from FCC licenses: who is licensed on the frequencies
// the survey heard, or a system's, near where the user is. Optional and only
// on request: it says why it wants a location, what is sent where, and that
// everything works the same without it. The recorder (or the browser build's
// worker) asks the trunkrecorder.pro API (POST /api/v1/fcc/identify).

import { useEffect, useState } from "react";
import { identifySurvey, licensees, lookupLicenses, setFcc, siteFrequencies, type Licensee, type LicenseLookup } from "./controller.ts";
import type { LicenseResultRow, SurveyLicense, System } from "./protocol.ts";

const LOCATION_KEY = "trp.surveyLocation";
const RADIUS_KEY = "trp.surveyRadiusKm";
export const RADII_KM = [10, 25, 50, 100, 150];

function load(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}
function save(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Storage blocked: asked again next time.
  }
}

/** "38.9072, -77.0369" (as Google Maps copies it), or "38.9072 -77.0369": a latitude and longitude, or null. */
export function parseLatLon(text: string): { lat: number; lon: number } | null {
  const m = /^\s*(-?\d+(?:\.\d+)?)\s*[,\s]\s*(-?\d+(?:\.\d+)?)\s*$/.exec(text);
  if (!m) return null;
  const lat = Number(m[1]);
  const lon = Number(m[2]);
  return Math.abs(lat) <= 90 && Math.abs(lon) <= 180 ? { lat, lon } : null;
}

const fmt = (lat: number, lon: number) => `${lat.toFixed(4)}, ${lon.toFixed(4)}`;

/** Where to look from, as the user gave it (this browser remembers it). */
export interface Where {
  text: string;
  radiusKm: number;
}

/** The location and distance last used in this browser. */
export function savedWhere(): Where {
  const p = parseLatLon(load(LOCATION_KEY) ?? "");
  const r = Number(load(RADIUS_KEY));
  return { text: p ? fmt(p.lat, p.lon) : "", radiusKm: RADII_KM.includes(r) ? r : 50 };
}

/** Its point and radius (m), remembered for next time; null when it isn't a location. */
export function takeWhere(w: Where): { lat: number; lon: number; radiusM: number } | null {
  const p = parseLatLon(w.text);
  if (!p) return null;
  save(LOCATION_KEY, fmt(p.lat, p.lon));
  save(RADIUS_KEY, String(w.radiusKm));
  return { ...p, radiusM: w.radiusKm * 1000 };
}

/** Why a location is wanted, what is sent where, and that it's fine to say no. */
export function WhyLocation(props: { what: string }) {
  return (
    <>
      <p className="small">
        FCC licenses are tied to where the transmitters are, so to name {props.what} we look up who is licensed on each frequency within a
        distance of you. For that we need your location.
      </p>
      <p className="small muted">
        The frequencies and the location you give are sent to <b>trunkrecorder.pro</b>, which answers from the FCC's license database. Nothing
        is kept there. If you'd rather not share a location, no lookup is made, and everything works exactly the same without it.
      </p>
    </>
  );
}

type Locate = { state: "idle" } | { state: "locating" } | { state: "located"; accuracyM: number } | { state: "failed"; why: string };

/** The location: the browser's (at once if this page may already have it), or typed; and how far to look. */
export function WhereFields(props: { value: Where; onChange: (w: Where) => void }) {
  const { value: w, onChange } = props;
  const [locate, setLocate] = useState<Locate>({ state: "idle" });
  const point = parseLatLon(w.text);
  const geo = typeof navigator !== "undefined" && "geolocation" in navigator;

  const useMine = () => {
    if (!geo) return setLocate({ state: "failed", why: "This browser can't tell where it is. Type a location instead." });
    if (typeof window !== "undefined" && !window.isSecureContext) {
      return setLocate({ state: "failed", why: "The browser only shares a location with pages on HTTPS or localhost. Type a location instead." });
    }
    setLocate({ state: "locating" });
    navigator.geolocation.getCurrentPosition(
      (p) => {
        onChange({ ...w, text: fmt(p.coords.latitude, p.coords.longitude) });
        setLocate({ state: "located", accuracyM: p.coords.accuracy });
      },
      (e) =>
        setLocate({
          state: "failed",
          why: e.code === e.PERMISSION_DENIED ? "Location sharing is turned off for this page. Type a location instead." : `${e.message || "No location"}. Type a location instead.`,
        }),
      { enableHighAccuracy: false, timeout: 10_000, maximumAge: 600_000 },
    );
  };

  // Already allowed: find it at once (no prompt is shown for this).
  useEffect(() => {
    if (!geo || typeof navigator.permissions?.query !== "function") return;
    navigator.permissions
      .query({ name: "geolocation" })
      .then((p) => {
        if (p.state === "granted") useMine();
      })
      .catch(() => {});
  }, []);

  return (
    <div className="stack">
      <div className="row">
        <button className="btn primary" onClick={useMine} disabled={locate.state === "locating"}>
          {locate.state === "locating" ? "Finding you…" : "Use my location"}
        </button>
        {locate.state === "located" && <span className="muted small">{`Located (within ${Math.round(locate.accuracyM)} m)`}</span>}
      </div>
      {locate.state === "failed" && <p className="warn small">{locate.why}</p>}
      <label className="field">
        <span className="field-label">Or type it: latitude, longitude</span>
        <input className="mono" value={w.text} placeholder="38.9072, -77.0369" onChange={(e) => onChange({ ...w, text: e.target.value })} />
        <span className="field-hint">{w.text && !point ? "Two numbers, like 38.9072, -77.0369 (copied from a map)." : "Right-click a spot in Google Maps to copy its numbers."}</span>
      </label>
      <label className="field">
        <span className="field-label">Look for licenses within</span>
        <select value={w.radiusKm} onChange={(e) => onChange({ ...w, radiusKm: Number(e.target.value) })}>
          {RADII_KM.map((r) => (
            <option key={r} value={r}>
              {`${r} km (${Math.round(r * 0.621)} miles)`}
            </option>
          ))}
        </select>
        <span className="field-hint">Wide-area systems can have towers 50 km or more away.</span>
      </label>
    </div>
  );
}

/** A dialog over the page: Escape or a click outside closes it. */
function Modal(props: { title: string; onClose: () => void; children: React.ReactNode }) {
  return (
    <div className="modal-back" role="dialog" aria-modal="true" aria-label={props.title} onClick={props.onClose} onKeyDown={(e) => e.key === "Escape" && props.onClose()} tabIndex={-1} ref={(el) => el?.focus()}>
      <div className="modal stack" onClick={(e) => e.stopPropagation()}>
        <h3>{props.title}</h3>
        {props.children}
      </div>
    </div>
  );
}

/** The licenses found within half a 6.25 kHz channel of `hz`, nearest first. */
export function licensesOn(l: LicenseLookup | null, hz: number): LicenseResultRow | null {
  if (!l || l.stage !== "done") return null;
  return l.results.find((r) => Math.abs(r.freqHz - hz) <= 3125) ?? null;
}

const distance = (m: number | null, dir: string | null) => (m === null ? "" : `${m < 10_000 ? (m / 1000).toFixed(1) : Math.round(m / 1000)} km ${dir ?? ""}`.trim());

/** The table cell: the nearest licensee, its call sign and how far; the others under a toggle. */
export function LicenseCell(props: { found: LicenseResultRow | null }) {
  const f = props.found;
  if (!f) return null;
  if (!f.licenses.length) return <span className="muted small">none nearby</span>;
  const [top, ...rest] = f.licenses;
  const line = (x: SurveyLicense) => (
    <>
      {x.licensee ?? "?"} <span className="mono">{x.callSign}</span>
      <span className="muted"> · {[x.serviceCode, distance(x.distanceM, x.bearingCardinal)].filter(Boolean).join(" · ")}</span>
    </>
  );
  return (
    <div className="small license" title={top.serviceDesc ?? undefined}>
      {line(top)}
      {f.historical && <span className="tag" title="No active license on this frequency nearby: these have expired or been cancelled">lapsed</span>}
      {rest.length > 0 && (
        <details>
          <summary className="muted">{`${rest.length} more`}</summary>
          {rest.map((x, i) => (
            <div key={i}>{line(x)}</div>
          ))}
        </details>
      )}
    </div>
  );
}

/** How a lookup is going, in a line. */
export function LookupStatus(props: { l: LicenseLookup | null }) {
  const l = props.l;
  if (!l) return null;
  if (l.stage === "running") return <span className="muted small">Looking up licenses…</span>;
  if (l.stage === "error") return <span className="warn small">{l.error}</span>;
  const found = l.results.filter((r) => r.licenses.length).length;
  return <span className="muted small">{`Licenses found for ${found} of ${l.results.length} frequenc${l.results.length === 1 ? "y" : "ies"} within ${Math.round(l.radiusM / 1000)} km`}</span>;
}

/** The survey's button: asks where, then looks up what it found. */
export function IdentifyBar(props: { licenses: LicenseLookup | null; canIdentify: boolean }) {
  const [open, setOpen] = useState(false);
  const [where, setWhere] = useState(savedWhere);
  const go = () => {
    const p = takeWhere(where);
    if (!p) return;
    identifySurvey(p.lat, p.lon, p.radiusM);
    setOpen(false);
  };
  return (
    <div className="row">
      <button className="btn" disabled={!props.canIdentify || props.licenses?.stage === "running"} onClick={() => setOpen(true)} title="Look up FCC licenses for the signals found (optional)">
        Identify surveyed sites
      </button>
      <LookupStatus l={props.licenses} />
      {open && (
        <Modal title="Identify surveyed sites" onClose={() => setOpen(false)}>
          <WhyLocation what="the systems you found" />
          <WhereFields value={where} onChange={setWhere} />
          <div className="row">
            <span className="spacer" />
            <button className="btn ghost" onClick={() => setOpen(false)}>
              Not now
            </button>
            <button className="btn primary" disabled={!parseLatLon(where.text)} onClick={go}>
              Identify
            </button>
          </div>
        </Modal>
      )}
    </div>
  );
}

/** A licensee to choose: who, its call sign, on how many of the frequencies, and how far. */
function LicenseeRow(props: { x: Licensee; of: number; current: boolean; onUse: () => void }) {
  const { x } = props;
  return (
    <li className="row licensee">
      <span className="stack-tight">
        <span>
          {x.fccName} <span className="mono">{x.fccCallSign}</span>
          {x.lapsed && <span className="tag">lapsed</span>}
        </span>
        <span className="muted small">{[`on ${x.frequencies} of ${props.of} frequencies`, distance(x.distanceM, x.bearingCardinal), x.service].filter(Boolean).join(" · ")}</span>
      </span>
      <span className="spacer" />
      {props.current ? (
        <span className="muted small">in use</span>
      ) : (
        <button className="btn small" onClick={props.onUse}>
          Use
        </button>
      )}
    </li>
  );
}

/** Setup's Identify on a system (`site` null) or one of its sites: where, then who is licensed on its frequencies, to keep as its FCC name and call sign. */
export function IdentifyFcc(props: { sys: System; system: number; site: number | null }) {
  const [open, setOpen] = useState(false);
  const [where, setWhere] = useState(savedWhere);
  const [l, setL] = useState<LicenseLookup | null>(null);
  const sites = props.site === null ? props.sys.sites : [props.sys.sites[props.site]].filter(Boolean);
  const signals = siteFrequencies(sites);
  const target = props.site === null ? props.sys : props.sys.sites[props.site];
  const close = () => (setOpen(false), setL(null));
  const go = () => {
    const p = takeWhere(where);
    if (!p) return;
    const asked: LicenseLookup = { stage: "running", ...p, results: [], error: null };
    setL(asked);
    void lookupLicenses(p.lat, p.lon, p.radiusM, signals).then((r) => setL({ ...asked, stage: r.error ? "error" : "done", ...r }));
  };
  const found = l?.stage === "done" ? licensees(l.results, signals.map((x) => x.freqHz)) : [];
  const what = props.site === null ? props.sys.name || props.sys.shortName : "this site";
  return (
    <>
      <button className="btn ghost small" disabled={!signals.length} onClick={() => setOpen(true)} title="Look up who holds its FCC license (optional)">
        Identify
      </button>
      {open && (
        <Modal title={`Identify ${what}`} onClose={close}>
          {!l || l.stage === "error" ? (
            <>
              <WhyLocation what={what} />
              <WhereFields value={where} onChange={setWhere} />
              {l?.stage === "error" && <div className="banner bad small">{l.error}</div>}
              <div className="row">
                <span className="spacer" />
                <button className="btn ghost" onClick={close}>
                  Not now
                </button>
                <button className="btn primary" disabled={!parseLatLon(where.text)} onClick={go}>
                  Identify
                </button>
              </div>
            </>
          ) : l.stage === "running" ? (
            <p className="muted">{`Looking up ${signals.length} frequenc${signals.length === 1 ? "y" : "ies"}…`}</p>
          ) : (
            <>
              {found.length ? (
                <>
                  <p className="small">{`Licensed on its ${signals.length} frequenc${signals.length === 1 ? "y" : "ies"} within ${Math.round(l.radiusM / 1000)} km. Use one as its FCC name and call sign:`}</p>
                  <ul className="licensees">
                    {found.slice(0, 8).map((x) => (
                      <LicenseeRow
                        key={`${x.fccName}|${x.fccCallSign}`}
                        x={x}
                        of={signals.length}
                        current={x.fccCallSign === target?.fccCallSign}
                        onUse={() => (setFcc(props.system, props.site, x.fccName, x.fccCallSign), close())}
                      />
                    ))}
                  </ul>
                </>
              ) : (
                <p className="small">{`Nobody is licensed on its frequencies within ${Math.round(l.radiusM / 1000)} km. Try farther, or check its frequencies.`}</p>
              )}
              <div className="row">
                <button className="btn ghost" onClick={() => setL(null)}>
                  Look again
                </button>
                <span className="spacer" />
                <button className="btn ghost" onClick={close}>
                  Close
                </button>
              </div>
            </>
          )}
        </Modal>
      )}
    </>
  );
}
