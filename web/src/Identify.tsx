// "Identify surveyed sites": who holds the FCC licenses on the frequencies the
// survey heard, near where the user is. Optional and only on request: the
// dialog says why it wants a location, what is sent where, and that the
// survey works the same without it. The recorder (or the browser build's
// worker) asks the trunkrecorder.pro API (POST /api/v1/fcc/identify); the
// answer comes back in the survey snapshot's `licenses`.

import { useEffect, useState } from "react";
import { downloadSurvey, surveyIdentify } from "./controller.ts";
import type { SurveyLicense, SurveyLicenses } from "./protocol.ts";

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

/** The licenses found within 3.125 kHz of `hz` (half a 6.25 kHz channel), nearest first. */
export function licensesOn(l: SurveyLicenses | undefined, hz: number): { historical: boolean; licenses: SurveyLicense[] } | null {
  if (!l || l.stage === "idle") return null;
  return l.results.find((r) => Math.abs(r.freqHz - hz) <= 3125) ?? null;
}

const distance = (x: SurveyLicense) => (x.distanceM === null ? "" : `${x.distanceM < 10_000 ? (x.distanceM / 1000).toFixed(1) : Math.round(x.distanceM / 1000)} km ${x.bearingCardinal ?? ""}`.trim());

/** The table cell: the nearest licensee, its call sign and how far; the others under a toggle. */
export function LicenseCell(props: { found: { historical: boolean; licenses: SurveyLicense[] } | null }) {
  const f = props.found;
  if (!f) return null;
  if (!f.licenses.length) return <span className="muted small">none nearby</span>;
  const [top, ...rest] = f.licenses;
  const line = (x: SurveyLicense) => (
    <>
      {x.licensee ?? "?"} <span className="mono">{x.callSign}</span>
      <span className="muted"> · {[x.serviceCode, distance(x)].filter(Boolean).join(" · ")}</span>
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

/** The button, how the lookup is going, and the survey download. */
export function IdentifyBar(props: { licenses: SurveyLicenses | undefined; canIdentify: boolean }) {
  const [open, setOpen] = useState(false);
  const l = props.licenses;
  const found = l && l.stage === "done" ? l.results.filter((r) => r.licenses.length).length : 0;
  return (
    <div className="stack">
      <div className="row">
        <button className="btn" disabled={!props.canIdentify || l?.stage === "running"} onClick={() => setOpen(true)} title="Look up FCC licenses for the signals found (optional)">
          Identify surveyed sites
        </button>
        <button className="btn ghost small" onClick={downloadSurvey} title="What was found, as a JSON file">
          Download survey
        </button>
        <span className="spacer" />
        {l?.stage === "running" && <span className="muted small">Looking up licenses…</span>}
        {l?.stage === "done" && (
          <span className="muted small">
            Licenses found for {found} of {l.results.length} frequenc{l.results.length === 1 ? "y" : "ies"} within {Math.round(l.radiusM / 1000)} km
          </span>
        )}
      </div>
      {l?.stage === "error" && <div className="banner bad small">{l.error}</div>}
      {open && <IdentifyDialog onClose={() => setOpen(false)} />}
    </div>
  );
}

type Locate = { state: "idle" } | { state: "locating" } | { state: "located"; accuracyM: number } | { state: "failed"; why: string };

/** Why we want a location, and getting one: the browser's, or typed. */
export function IdentifyDialog(props: { onClose: () => void }) {
  const saved = parseLatLon(load(LOCATION_KEY) ?? "");
  const [text, setText] = useState(saved ? fmt(saved.lat, saved.lon) : "");
  const [radiusKm, setRadiusKm] = useState(() => {
    const r = Number(load(RADIUS_KEY));
    return RADII_KM.includes(r) ? r : 50;
  });
  const [locate, setLocate] = useState<Locate>({ state: "idle" });
  const point = parseLatLon(text);
  const geo = typeof navigator !== "undefined" && "geolocation" in navigator;

  const useMine = () => {
    if (!geo) return setLocate({ state: "failed", why: "This browser can't tell where it is. Type a location instead." });
    if (typeof window !== "undefined" && !window.isSecureContext) {
      return setLocate({ state: "failed", why: "The browser only shares a location with pages on HTTPS or localhost. Type a location instead." });
    }
    setLocate({ state: "locating" });
    navigator.geolocation.getCurrentPosition(
      (p) => {
        setText(fmt(p.coords.latitude, p.coords.longitude));
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

  const identify = () => {
    if (!point) return;
    save(LOCATION_KEY, fmt(point.lat, point.lon));
    save(RADIUS_KEY, String(radiusKm));
    surveyIdentify(point.lat, point.lon, radiusKm * 1000);
    props.onClose();
  };

  return (
    <div className="modal-back" role="dialog" aria-modal="true" aria-labelledby="identify-title" onClick={props.onClose} onKeyDown={(e) => e.key === "Escape" && props.onClose()} tabIndex={-1} ref={(el) => el?.focus()}>
      <div className="modal stack" onClick={(e) => e.stopPropagation()}>
        <h3 id="identify-title">Identify surveyed sites</h3>
        <p className="small">
          FCC licenses are tied to where the transmitters are, so to name the systems you found we look up who is licensed on each frequency
          within a distance of you. For that we need your location.
        </p>
        <p className="small muted">
          The frequencies found and the location you give are sent to <b>trunkrecorder.pro</b>, which answers from the FCC's license database.
          Nothing is kept there. If you'd rather not share a location, no lookup is made, and the survey works exactly the same without it.
        </p>
        <div className="row">
          <button className="btn primary" onClick={useMine} disabled={locate.state === "locating"}>
            {locate.state === "locating" ? "Finding you…" : "Use my location"}
          </button>
          {locate.state === "located" && <span className="muted small">Located (within {Math.round(locate.accuracyM)} m)</span>}
        </div>
        {locate.state === "failed" && <p className="warn small">{locate.why}</p>}
        <label className="field">
          <span className="field-label">Or type it: latitude, longitude</span>
          <input className="mono" value={text} placeholder="38.9072, -77.0369" onChange={(e) => setText(e.target.value)} />
          <span className="field-hint">{text && !point ? "Two numbers, like 38.9072, -77.0369 (copied from a map)." : "Right-click a spot in Google Maps to copy its numbers."}</span>
        </label>
        <label className="field">
          <span className="field-label">Look for licenses within</span>
          <select value={radiusKm} onChange={(e) => setRadiusKm(Number(e.target.value))}>
            {RADII_KM.map((r) => (
              <option key={r} value={r}>
                {`${r} km (${Math.round(r * 0.621)} miles)`}
              </option>
            ))}
          </select>
          <span className="field-hint">Wide-area systems can have towers 50 km or more away.</span>
        </label>
        <div className="row">
          <span className="spacer" />
          <button className="btn ghost" onClick={props.onClose}>
            Not now
          </button>
          <button className="btn primary" disabled={!point} onClick={identify}>
            Identify
          </button>
        </div>
      </div>
    </div>
  );
}
