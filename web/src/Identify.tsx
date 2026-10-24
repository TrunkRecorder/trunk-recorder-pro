// Identifying systems from FCC licenses: who is licensed on the frequencies
// the survey heard, or a system's, near where the user is. Optional and only
// on request: it says why it wants a location, what is sent where, and that
// everything works the same without it. The recorder (or the browser build's
// worker) asks the trunkrecorder.pro API (POST /api/v1/fcc/identify).

import { useEffect, useRef, useState } from "react";
import { fccKey, trpBase } from "./config.ts";
import { editFcc, findPlaces, identifySurvey, lookupCallSigns, lookupLicenses, lookupLicensesByFreq, mergeLookup, nameCallSigns, useApp, type FccTarget, type LicenseLookup, type Place } from "./controller.ts";
import { LocationMap } from "./LocationMap.tsx";
import type { FccLicense, FccLicenses, LicenseResultRow, SurveyLicense } from "./protocol.ts";

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

/** The location: the browser's (at once if this page may already have it), a place found, or where the map's circle is; and how far to look. */
export function WhereFields(props: { value: Where; onChange: (w: Where) => void }) {
  const { value: w, onChange } = props;
  const app = useApp();
  const base = trpBase(app.config);
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

  // A place by name ("Arlington, VA"), ZIP code, or coordinates: the directory's place search.
  const [find, setFind] = useState("");
  const [found, setFound] = useState<Place[] | null>(null);
  const [finding, setFinding] = useState(false);
  const search = async () => {
    setFound(null);
    const p = parseLatLon(find);
    if (p) return onChange({ ...w, text: fmt(p.lat, p.lon) });
    if (find.trim().length < 3) return;
    setFinding(true);
    const places = await findPlaces(find.trim());
    setFinding(false);
    if (places.length === 1) onChange({ ...w, text: fmt(places[0].lat, places[0].lon) });
    else setFound(places);
  };

  return (
    <div className="stack">
      <div className="row">
        <button className="btn primary" onClick={useMine} disabled={locate.state === "locating"}>
          {locate.state === "locating" ? "Finding you…" : "Use my location"}
        </button>
        {locate.state === "located" && <span className="muted small">{`Located (within ${Math.round(locate.accuracyM)} m)`}</span>}
      </div>
      {locate.state === "failed" && <p className="warn small">{locate.why}</p>}
      <form
        className="row"
        onSubmit={(e) => {
          e.preventDefault();
          void search();
        }}
      >
        <input className="search" value={find} onChange={(e) => setFind(e.target.value)} placeholder="Or find a place: City, ST · ZIP code · coordinates" aria-label="Find a place" style={{ flex: 1 }} />
        <button className="btn" disabled={finding}>
          {finding ? "Finding…" : "Find"}
        </button>
      </form>
      {found && (
        <div className="row small">
          {!found.length && <span className="muted">No place by that name. Try "City, ST" or a ZIP code.</span>}
          {found.slice(0, 6).map((x) => (
            <button key={`${x.name}${x.state}`} className="btn ghost small" onClick={() => (setFound(null), onChange({ ...w, text: fmt(x.lat, x.lon) }))}>
              {`${x.name}, ${x.state}`}
            </button>
          ))}
        </div>
      )}
      <LocationMap base={base} at={point ? [point.lat, point.lon] : null} onMove={([lat, lon]) => onChange({ ...w, text: fmt(lat, lon) })} />
      <span className="field-hint">
        {point ? `Move the map so the circle is roughly where you are: only your general area is needed (${fmt(point.lat, point.lon)}).` : "Move the map so the circle is roughly where you are: only your general area is needed."}
      </span>
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
export function Modal(props: { title: string; onClose: () => void; children: React.ReactNode }) {
  // Focus once on open: an inline ref callback runs again on every render and would pull focus out of the fields.
  const back = useRef<HTMLDivElement>(null);
  useEffect(() => back.current?.focus(), []);
  return (
    <div className="modal-back" role="dialog" aria-modal="true" aria-label={props.title} onClick={props.onClose} onKeyDown={(e) => e.key === "Escape" && props.onClose()} tabIndex={-1} ref={back}>
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

const mhzOf = (k: string) => (Number(k) / 1e6).toFixed(4);
const FROM: Record<string, string> = { air: "heard", lookup: "lookup" };

/** A system's FCC licenses by frequency, to edit: what's typed is the user's (never replaced). */
export function FccTable(props: { target: FccTarget; fcc: FccLicenses | undefined }) {
  const rows = Object.entries(props.fcc ?? {}).sort(([a], [b]) => Number(a) - Number(b));
  const [mhz, setMhz] = useState("");
  const set = (k: string, l: FccLicense | null) =>
    editFcc(props.target, (f) => {
      if (l) f[k] = l;
      else delete f[k];
    });
  const hz = Math.round(Number(mhz) * 1e6);
  const add = () => {
    if (!(hz > 0)) return;
    const k = fccKey(hz);
    if (!props.fcc?.[k]) set(k, { callSign: "" });
    setMhz("");
  };
  return (
    <div className="stack">
      {rows.length > 0 && (
        <div className="table-wrap">
          <table className="calls fcc-table">
            <thead>
              <tr>
                <th>MHz</th>
                <th>Call sign</th>
                <th>Licensee</th>
                <th />
                <th />
              </tr>
            </thead>
            <tbody>
              {rows.map(([k, l]) => (
                <tr key={k}>
                  <td className="mono">{mhzOf(k)}</td>
                  <td>
                    <input className="mono" value={l.callSign} aria-label={`Call sign on ${mhzOf(k)} MHz`} onChange={(e) => set(k, { ...l, callSign: e.target.value.toUpperCase().trim(), from: undefined })} />
                  </td>
                  <td>
                    <input value={l.name ?? ""} aria-label={`Licensee on ${mhzOf(k)} MHz`} onChange={(e) => set(k, { ...l, name: e.target.value || undefined, from: undefined })} />
                  </td>
                  <td className="muted small">{l.from ? FROM[l.from] : "yours"}</td>
                  <td className="actions">
                    <button className="btn ghost small" onClick={() => set(k, null)} aria-label={`Remove ${mhzOf(k)} MHz`}>
                      Remove
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <div className="row">
        <input className="mono" value={mhz} placeholder="MHz" aria-label="Frequency, MHz" onChange={(e) => setMhz(e.target.value)} onKeyDown={(e) => e.key === "Enter" && add()} />
        <button className="btn ghost small" disabled={!(hz > 0)} onClick={add}>
          Add a frequency
        </button>
      </div>
    </div>
  );
}

/** What Identify can do for a system: look up its frequencies (at most 64), and name the call signs it has without a licensee. */
function identifiable(freqs: number[], fcc: FccLicenses | undefined) {
  return {
    signals: freqs.slice(0, 64).map((freqHz) => ({ freqHz })),
    unnamed: [...new Set(Object.values(fcc ?? {}).filter((x) => x.callSign && !x.name).map((x) => x.callSign))],
  };
}

/** Setup's Identify on a system or a conventional system: names for the call signs heard on the air (no location), and who is licensed on its frequencies near where the user is, into its FCC licenses. */
export function IdentifyFcc(props: { target: FccTarget; what: string; freqs: number[]; fcc: FccLicenses | undefined }) {
  const [open, setOpen] = useState(false);
  const { signals, unnamed } = identifiable(props.freqs, props.fcc);
  return (
    <>
      <button className="btn ghost small" disabled={!signals.length && !unnamed.length} onClick={() => setOpen(true)} title="Look up who holds its FCC licenses (optional)">
        Identify
      </button>
      {open && <IdentifyDialog {...props} onClose={() => setOpen(false)} />}
    </>
  );
}

/** Identify's dialog: what is kept goes into the config at once (`onClose` can read it there). */
export function IdentifyDialog(props: { target: FccTarget; what: string; freqs: number[]; fcc: FccLicenses | undefined; intro?: React.ReactNode; onClose: () => void }) {
  const [where, setWhere] = useState(savedWhere);
  const [l, setL] = useState<LicenseLookup | null>(null);
  const [named, setNamed] = useState<string | null>(null);
  const { signals, unnamed } = identifiable(props.freqs, props.fcc);
  const close = props.onClose;
  const nameThem = () => {
    setNamed("Looking them up…");
    void lookupCallSigns(unnamed).then((r) => {
      let n = 0;
      if (!r.error) editFcc(props.target, (f) => void (n = nameCallSigns(f, r.results)));
      setNamed(r.error ?? `Named ${n} of ${unnamed.length}.`);
    });
  };
  const go = () => {
    const p = takeWhere(where);
    if (!p) return;
    const asked: LicenseLookup = { stage: "running", ...p, results: [], error: null };
    setL(asked);
    void lookupLicenses(p.lat, p.lon, p.radiusM, signals).then((r) => setL({ ...asked, stage: r.error ? "error" : "done", ...r }));
  };
  const found = l?.stage === "done" ? lookupLicensesByFreq(l.results) : {};
  const merged = { ...(props.fcc ?? {}) };
  const changes = mergeLookup(merged, found);
  return (
    <Modal title={`Identify ${props.what}`} onClose={close}>
      {props.intro}
      {unnamed.length > 0 && (
        <div className="stack">
          <p className="small">{`${unnamed.length} call sign${unnamed.length === 1 ? "" : "s"} heard on the air ${unnamed.length === 1 ? "has" : "have"} no licensee yet. Looking ${unnamed.length === 1 ? "it" : "them"} up sends only the call sign${unnamed.length === 1 ? "" : "s"} to trunkrecorder.pro, no location.`}</p>
          <div className="row">
            <button className="btn" disabled={named === "Looking them up…"} onClick={nameThem}>
              Look up their licensees
            </button>
            {named && <span className="muted small">{named}</span>}
          </div>
        </div>
      )}
      {signals.length > 0 &&
        (!l || l.stage === "error" ? (
          <>
            <WhyLocation what={props.what} />
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
            {Object.keys(found).length ? (
              <>
                <p className="small">{`Licensed within ${Math.round(l.radiusM / 1000)} km, the nearest on each frequency (yours, and call signs heard on the air, stay):`}</p>
                <ul className="licensees">
                  {Object.entries(found)
                    .sort(([a], [b]) => Number(a) - Number(b))
                    .map(([k, x]) => (
                      <li key={k} className="row licensee small">
                        <span className="mono">{mhzOf(k)}</span>
                        <span className="mono">{x.callSign}</span>
                        <span>{x.name}</span>
                        <span className="spacer" />
                        <span className="muted">{merged[k] === x || (merged[k]?.name === x.name && props.fcc?.[k]?.name !== x.name) ? "new" : "kept"}</span>
                      </li>
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
              <button
                className="btn primary"
                disabled={!changes}
                onClick={() => {
                  editFcc(props.target, (f) => void mergeLookup(f, found));
                  close();
                }}
              >
                {changes ? `Keep ${changes}` : "Nothing new"}
              </button>
            </div>
          </>
        ))}
      {!signals.length && (
        <div className="row">
          <span className="spacer" />
          <button className="btn ghost" onClick={close}>
            Close
          </button>
        </div>
      )}
    </Modal>
  );
}
