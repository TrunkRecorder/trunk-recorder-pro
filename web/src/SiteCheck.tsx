// "Check sites": which sites of a RadioReference sites export can be heard
// from here, checked with a spare radio while recording carries on with the
// others. The recorder does the work (crates/trunk-pro/src/sitecheck.rs) and
// reports after every window; this starts it and shows it. The result is the
// export without the sites heard nothing of (to set the system up from), and
// a report of what each frequency showed.

import { useState } from "react";
import { formatMhz } from "./config.ts";
import { can, findRadios, refreshDevices, startSiteCheck, stopSiteCheck, useApp } from "./controller.ts";
import { Waterfall, type CcMark } from "./Waterfall.tsx";
import type { Config, Radios, Device, SiteCheckRadio, SiteCheckState, SiteCheckSystemType, SiteDecodeQuality, SiteVerdict } from "./protocol.ts";

const TYPES: { id: SiteCheckSystemType; label: string }[] = [
  { id: "p25", label: "P25" },
  { id: "smartnet", label: "SmartNet" },
  { id: "edacs", label: "EDACS" },
  { id: "dmr", label: "DMR (Capacity Plus, Connect Plus, Tier III)" },
  { id: "nxdn", label: "NXDN" },
];

const KINDS: { id: SiteCheckRadio["kind"]; label: string; idLabel: string }[] = [
  { id: "rtlsdr", label: "RTL-SDR", idLabel: "Serial" },
  { id: "airspy", label: "Airspy", idLabel: "Serial" },
  { id: "bladerf", label: "bladeRF", idLabel: "Serial" },
  { id: "soapy", label: "SoapySDR (SDRplay, HackRF …)", idLabel: "Device arguments" },
  { id: "usrp", label: "USRP", idLabel: "Device arguments" },
];

/** Each kind's sample rate when none is given (as the recorder's check-sites), and about the most it takes, MS/s. */
const RATES: Record<SiteCheckRadio["kind"], { def: number; max: number; hint: string }> = {
  rtlsdr: { def: 2.4, max: 3.2, hint: "an RTL-SDR tops out at 2.4 (3.2 drops samples)" },
  airspy: { def: 6, max: 10, hint: "Airspy R2: 2.5 or 10; Mini: 3 or 6" },
  bladerf: { def: 8, max: 61.44, hint: "bladeRF 2.0: up to 61.44 on USB 3; 20–40 is practical" },
  soapy: { def: 2, max: 61.44, hint: "SDRplay: up to 10; HackRF: up to 20" },
  usrp: { def: 8, max: 56, hint: "B200 / B210: up to 56" },
};

/** The share of a window the check uses (its edges roll off, as the recorder's survey::USABLE). */
const USABLE = 0.85;

/**
 * The frequencies a sites export has the check listen to (each row's `c` ones, else all of
 * them), Hz: RadioReference writes them with six decimals, MHz.
 */
export function checkedFreqs(csv: string): number[] {
  const out = new Set<number>();
  for (const line of csv.split(/\r?\n/).slice(1)) {
    const f = line.split(",").map((x) => x.trim().replace(/"/g, "")).filter((x) => /^\d{2,4}\.\d{6}c?$/.test(x));
    const marked = f.filter((x) => x.endsWith("c"));
    for (const x of marked.length ? marked : f) out.add(Math.round(parseFloat(x) * 1e6));
  }
  return [...out].sort((a, b) => a - b);
}

/** About how many windows the check needs at `rateHz` (before any ZIP radius leaves sites out). */
export function windowsFor(freqs: number[], rateHz: number): number {
  const span = rateHz * USABLE - 10_000;
  let n = 0;
  let from = -Infinity;
  for (const f of freqs) {
    if (f - from > span) {
      n++;
      from = f;
    }
  }
  return n;
}

/** How long to keep going round the windows (0: one pass). */
const LONG: { s: number; label: string }[] = [
  { s: 0, label: "One pass" },
  { s: 3600, label: "1 hour" },
  { s: 4 * 3600, label: "4 hours" },
  { s: 12 * 3600, label: "12 hours" },
  { s: 24 * 3600, label: "24 hours" },
];

const VERDICT_CLASS: Record<SiteVerdict, string> = {
  active: "ok",
  unconfirmed: "warn",
  "other site": "bad",
  inactive: "bad",
  "not checked": "",
  "out of range": "",
};

/** A radio the spare-radio list offers. */
interface Offer {
  key: string;
  radio: SiteCheckRadio;
  label: string;
  /** Why it can't be picked (the recorder uses it, another program has it). */
  taken: string | null;
  /** The only rates it runs at, Hz (an Airspy's, once known), or the span it runs in. */
  rates?: number[];
  rateRange?: [number, number];
}

function idOf(s: Config["sources"][number]): [string, string] | null {
  switch (s.type) {
    case "rtlsdr":
    case "airspy":
    case "bladerf":
      return [s.type, s.serial];
    case "soapy":
    case "usrp":
      return [s.type, s.args];
    default:
      return null;
  }
}

/** Which source of the recorder `r` is (or could be: no serial), or -1. */
export function sourceUsing(c: Config, r: SiteCheckRadio): number {
  return c.sources.findIndex((s) => {
    const k = idOf(s);
    if (!k || k[0] !== r.kind) return false;
    const [a, b] = [k[1].trim(), r.id.trim()];
    return a === b || a === "" || b === "";
  });
}

/** Every radio found: RTL-SDRs, and what the optional drivers found. */
export function offers(c: Config, devices: Device[], radios: Radios | null): Offer[] {
  const out: Offer[] = [];
  const add = (radio: SiteCheckRadio, label: string, busy?: string | null, found?: { rates?: number[]; rateRange?: [number, number] }) => {
    const used = sourceUsing(c, radio);
    out.push({ key: `${radio.kind}:${radio.id}`, radio, label, taken: used >= 0 ? `source ${used + 1} of the recorder` : busy ? busy : null, rates: found?.rates, rateRange: found?.rateRange });
  };
  for (const d of devices) add({ kind: "rtlsdr", id: d.serial }, `${d.product || "RTL-SDR"} · SN ${d.serial || "(none)"}`, d.busy);
  for (const d of radios?.airspy.devices ?? []) add({ kind: "airspy", id: d.serial ?? "" }, d.label, null, d);
  for (const d of radios?.bladerf.devices ?? []) add({ kind: "bladerf", id: d.serial ?? "" }, d.label, null, d);
  for (const d of radios?.soapy?.devices ?? []) if (d.driver !== "rtlsdr" && d.driver !== "airspy") add({ kind: "soapy", id: d.args }, `${d.label} (SoapySDR)`, null, d);
  for (const d of radios?.usrp.devices ?? []) add({ kind: "usrp", id: d.args ?? "" }, d.label, null, d);
  return out;
}

const pct = (x: number) => `${Math.round(x * 100)} %`;

/** How strong a peak (dB over the noise floor) is, for the meter: about 6 dB is barely there, 30 dB and up is strong. */
export function strengthOf(db: number): { word: string; cls: string; fill: number } {
  const fill = Math.max(0, Math.min(1, db / 35));
  if (db >= 25) return { word: "strong", cls: "ok", fill };
  if (db >= 12) return { word: "fair", cls: "ok", fill };
  if (db >= 6) return { word: "weak", cls: "warn", fill };
  return { word: "noise", cls: "", fill };
}

/** The colour each decode quality gets: green, yellow, red. */
const QUALITY_CLASS: Record<SiteDecodeQuality, string> = { good: "ok", marginal: "warn", poor: "bad" };
const QUALITY_LABEL: Record<SiteDecodeQuality, string> = { good: "good decode", marginal: "marginal", poor: "poor" };

/** A table row's class for how well it decoded (none until listened to). */
const rowOf = (q: SiteDecodeQuality | null) => (q ? `q-${q}` : "");

function Decode(props: { q: SiteDecodeQuality | null; share?: number | null }) {
  if (!props.q) return null;
  return (
    <span className={`chip ${QUALITY_CLASS[props.q]}`} title={props.share != null ? `${Math.round(props.share * 100)} % of its frames decoded` : undefined}>
      {QUALITY_LABEL[props.q]}
      {props.share != null ? ` · ${Math.round(props.share * 100)} %` : ""}
    </span>
  );
}

/** A small bar and the figure, coloured by how well it decoded (else by strength alone). */
function Strength(props: { db: number | null; q?: SiteDecodeQuality | null }) {
  if (props.db == null) return null;
  const s = strengthOf(props.db);
  const cls = props.q ? QUALITY_CLASS[props.q] : s.cls;
  return (
    <span className={`strength ${cls}`} title={`${props.db.toFixed(1)} dB over the noise floor at its strongest (${s.word})`}>
      <span className="strength-bar" aria-hidden="true">
        <span style={{ width: `${Math.round(s.fill * 100)}%` }} />
      </span>
      <span className="mono">{props.db.toFixed(0)} dB</span>
    </span>
  );
}

/** "45 s", "12 min", "3.5 h" (as the recorder says them). */
export function duration(s: number): string {
  if (s < 90) return `${Math.round(s)} s`;
  if (s < 5400) return `${Math.round(s / 60)} min`;
  return `${(s / 3600).toFixed(1)} h`;
}

function download(name: string, text: string) {
  const url = URL.createObjectURL(new Blob([text], { type: "text/csv" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

const stem = (name: string) => name.replace(/\.csv$/i, "");

export function SiteCheckPanel(props: { c: Config }) {
  const s = useApp();
  const { c } = props;
  const sc = s.siteCheck;
  const [open, setOpen] = useState(false);
  if (!can("drivers") && !can("service")) return null;
  const busy = sc.state === "running";
  return (
    <section className="panel survey site-check">
      <header className="panel-head">
        <h2>Check sites</h2>
        <div className="row">
          {busy && <span className="pill pill-running">checking</span>}
          {busy ? (
            <button className="btn ghost" onClick={stopSiteCheck} title="Stops listening; what was heard so far is kept and saved">
              Stop
            </button>
          ) : (
            <button className="btn ghost" onClick={() => setOpen(!open)} aria-expanded={open}>
              {open ? "Hide" : "Show"}
            </button>
          )}
        </div>
      </header>
      {(open || busy) && (
        <div className="stack">
          <p className="muted small">
            Which sites of a system can be heard from here: listens to each site's control channels (marked <span className="mono">c</span>) with a <b>spare radio</b> — one that isn't a source above
            — so recording keeps going. Then gives you the site list without the ones heard nothing of.
          </p>
          {!busy && <StartForm c={c} key={sc.state === "idle" ? "new" : "again"} />}
        </div>
      )}
      {sc.state !== "idle" && <Result sc={sc} />}
    </section>
  );
}

function StartForm(props: { c: Config }) {
  const s = useApp();
  const { c } = props;
  const [file, setFile] = useState<{ name: string; csv: string } | null>(null);
  const [type, setType] = useState<SiteCheckSystemType>("p25");
  const list = offers(c, s.devices, s.radios);
  const free = list.filter((o) => !o.taken);
  const [pick, setPick] = useState<string>("");
  const [manual, setManual] = useState<SiteCheckRadio>({ kind: "rtlsdr", id: "" });
  const [gain, setGain] = useState("");
  const [rate, setRate] = useState("");
  const [zip, setZip] = useState("");
  const [within, setWithin] = useState("40");
  const [seconds, setSeconds] = useState("");
  const [forS, setForS] = useState(0);
  const offer = pick === "other" ? undefined : (list.find((o) => o.key === pick) ?? free[0]);
  const chosen = pick === "other" ? manual : offer?.radio;
  const usedBy = chosen ? sourceUsing(c, chosen) : -1;
  const listen = Number(seconds) > 0 ? Number(seconds) : type === "dmr" ? 120 : 4;
  const zipOk = zip.trim() === "" || /^\d{5}$/.test(zip.trim());
  // What this radio says it runs at, once known: an Airspy only its few rates (R2 10 / 2.5, Mini 6 / 3).
  const only = offer?.rates?.length ? offer.rates.map((r) => r / 1e6) : null;
  const span = offer?.rateRange ? offer.rateRange.map((r) => r / 1e6) : null;
  const base = RATES[chosen?.kind ?? "rtlsdr"];
  const rates = only
    ? { def: Math.max(...only), max: Math.max(...only), hint: `this ${offer!.label.split(" · ")[0]} runs at ${only.join(" or ")} only` }
    : span
      ? { def: Math.min(Math.max(base.def, span[0]), span[1]), max: span[1], hint: `this radio runs at ${span[0]}–${span[1]}` }
      : base;
  const msps = rate.trim() === "" ? rates.def : Number(rate);
  const rateOk = rate.trim() === "" || (Number.isFinite(msps) && msps >= 0.25 && msps <= rates.max && (!only || only.some((r) => Math.abs(r - msps) < 1e-6)));
  const freqs = file ? checkedFreqs(file.csv) : [];
  const windows = rateOk && freqs.length ? windowsFor(freqs, msps * 1e6) : 0;
  const spanMhz = freqs.length ? (freqs[freqs.length - 1] - freqs[0]) / 1e6 : 0;
  const ready = !!file && !!chosen && usedBy < 0 && zipOk && rateOk && s.connected;
  const start = () => {
    if (!file || !chosen) return;
    const g = gain.trim() === "" ? undefined : Number(gain);
    startSiteCheck({
      csv: file.csv,
      name: file.name,
      systemType: type,
      radio: { ...chosen, ...(g !== undefined && Number.isFinite(g) ? { gain: g } : {}), ...(rate.trim() !== "" || rates.def !== base.def ? { rateHz: Math.round(msps * 1e6) } : {}) },
      ...(Number(seconds) > 0 ? { seconds: Number(seconds) } : {}),
      ...(forS > 0 ? { forS } : {}),
      ...(zip.trim() ? { zip: zip.trim(), withinMiles: Number(within) > 0 ? Number(within) : 40 } : {}),
    });
  };
  const kind = KINDS.find((k) => k.id === manual.kind)!;
  return (
    <div className="stack">
      <div className="grid2">
        <label className="field">
          <span className="field-label">Sites file</span>
          <input
            type="file"
            accept=".csv,text/csv"
            onChange={async (e) => {
              const f = e.target.files?.[0];
              setFile(f ? { name: f.name, csv: await f.text() } : null);
            }}
          />
          <span className="field-hint">RadioReference: the system's page → Download → Sites (trs_sites_NNNN.csv)</span>
        </label>
        <label className="field">
          <span className="field-label">Kind of system</span>
          <select value={type} onChange={(e) => setType(e.target.value as SiteCheckSystemType)}>
            {TYPES.map((t) => (
              <option key={t.id} value={t.id}>
                {t.label}
              </option>
            ))}
          </select>
        </label>
        <label className="field">
          <span className="field-label">Spare radio</span>
          <select value={pick || free[0]?.key || "other"} onChange={(e) => setPick(e.target.value)}>
            {list.map((o) => (
              <option key={o.key} value={o.key} disabled={!!o.taken}>
                {o.label}
                {o.taken ? ` — ${o.taken}` : ""}
              </option>
            ))}
            <option value="other">Another radio (type it in)…</option>
          </select>
          <span className="field-hint">
            Not listed?{" "}
            <button className="linkish" onClick={() => (can("drivers") ? findRadios() : refreshDevices())} disabled={s.findingRadios}>
              {s.findingRadios ? "Looking…" : "Look for radios"}
            </button>
          </span>
        </label>
        <label className="field">
          <span className="field-label">Gain</span>
          <input type="number" placeholder={chosen?.kind === "airspy" ? "14 (linearity step 0–21)" : chosen?.kind === "rtlsdr" || chosen?.kind === "soapy" ? "automatic (AGC)" : "driver default"} value={gain} onChange={(e) => setGain(e.target.value)} />
          <span className="field-hint">{chosen?.kind === "airspy" ? "Airspy: a linearity step, 0–21" : "dB; leave empty for automatic"}</span>
        </label>
        <label className="field">
          <span className="field-label">Sample rate (MS/s)</span>
          <input type="number" min={0.25} max={rates.max} step="any" placeholder={`${rates.def} (default)`} value={rate} onChange={(e) => setRate(e.target.value)} />
          <span className="field-hint">
            {!rateOk
              ? only
                ? `This radio runs at ${only.join(" or ")} MS/s only`
                : `Between 0.25 and ${rates.max} for this radio`
              : `Sees about ${(msps * USABLE).toFixed(1)} MHz at once — wider means fewer windows, more CPU and USB. ${rates.hint[0].toUpperCase()}${rates.hint.slice(1)}.`}
          </span>
        </label>
        {freqs.length > 0 && rateOk && (
          <div className="field">
            <span className="field-label">Windows</span>
            <span className={windows === 1 ? "q-good-text" : ""}>
              {windows === 1 ? "1 — every frequency listened to all the time" : `about ${windows}`}
            </span>
            <span className="field-hint">
              {freqs.length} frequencies over {spanMhz.toFixed(1)} MHz
              {windows > 1 ? `; about ${Math.ceil(spanMhz / USABLE + 0.5)} MS/s would see them all at once` : ""}
              {zip.trim() ? " (before the ZIP radius, which may need fewer)" : ""}
            </span>
          </div>
        )}
      </div>
      {(pick === "other" || (!free.length && !pick)) && (
        <div className="grid2">
          <label className="field">
            <span className="field-label">Radio kind</span>
            <select value={manual.kind} onChange={(e) => setManual({ ...manual, kind: e.target.value as SiteCheckRadio["kind"] })}>
              {KINDS.map((k) => (
                <option key={k.id} value={k.id}>
                  {k.label}
                </option>
              ))}
            </select>
          </label>
          <label className="field">
            <span className="field-label">{kind.idLabel}</span>
            <input
              className="mono"
              value={manual.id}
              placeholder={manual.kind === "soapy" ? "driver=sdrplay,serial=…,mode=ST" : manual.kind === "usrp" ? "serial=…" : "00000002"}
              onChange={(e) => {
                setPick("other");
                setManual({ ...manual, id: e.target.value });
              }}
            />
          </label>
        </div>
      )}
      <div className="grid2">
        <label className="field">
          <span className="field-label">Your ZIP code</span>
          <input inputMode="numeric" maxLength={5} placeholder="optional, e.g. 12601" value={zip} onChange={(e) => setZip(e.target.value)} />
          <span className="field-hint">{zipOk ? "Leaves out the sites farther away than below (not listened to)" : "5 digits"}</span>
        </label>
        <label className="field">
          <span className="field-label">Within (miles)</span>
          <input type="number" min={1} value={within} disabled={!zip.trim()} onChange={(e) => setWithin(e.target.value)} />
        </label>
        <label className="field">
          <span className="field-label">Listen per window (s)</span>
          <input type="number" min={1} placeholder={String(type === "dmr" ? 120 : 4)} value={seconds} onChange={(e) => setSeconds(e.target.value)} />
          <span className="field-hint">{type === "dmr" ? "DMR repeaters can be quiet between calls: 120 s by default" : "A control channel never stops: 4 s is plenty"}</span>
        </label>
        <label className="field">
          <span className="field-label">Keep going</span>
          <select value={forS} onChange={(e) => setForS(Number(e.target.value))}>
            {LONG.map((l) => (
              <option key={l.s} value={l.s}>
                {l.label}
              </option>
            ))}
          </select>
          <span className="field-hint">Goes round all the windows again and again: a quiet DMR site gets more chances. Results update as it goes.</span>
        </label>
      </div>
      {usedBy >= 0 && <p className="warn small">That's source {usedBy + 1} of the recorder (or could be — give its serial). Two programs can't open one radio: pick a spare one.</p>}
      <div className="row">
        <button className="btn primary" disabled={!ready} onClick={start}>
          Check sites
        </button>
        <span className="muted small">
          {file ? `${file.name} · ` : ""}
          {listen} s per window{forS > 0 ? ` · for ${duration(forS)}` : ""}
        </span>
      </div>
    </div>
  );
}

/** The markers on the waterfall: each frequency listened to, coloured by how well it decoded so far. */
const MARK_COLOR: Record<SiteDecodeQuality | "none", string> = { good: "var(--ok)", marginal: "var(--warn)", poor: "var(--bad)", none: "var(--muted)" };

function Result(props: { sc: Exclude<SiteCheckState, { state: "idle" }> }) {
  const { sc } = props;
  const spectrum = useApp().siteCheckSpectrum;
  const [freqsOpen, setFreqsOpen] = useState(false);
  const running = sc.state === "running";
  const counts = sc.sites.reduce<Record<string, number>>((m, x) => ((m[x.verdict] = (m[x.verdict] ?? 0) + 1), m), {});
  return (
    <div className="stack">
      {sc.error && <div className="banner bad small">The radio: {sc.error}</div>}
      <div className="small">
        <b>{sc.name}</b> · {sc.summary}
      </div>
      <div className="muted small">
        Radio <span className="mono">{sc.radio}</span> · gain {sc.gain} · {(sc.rateHz / 1e6).toFixed(1)} MS/s
      </div>
      {sc.note && <div className="muted small">{sc.note[0].toUpperCase() + sc.note.slice(1)}.</div>}
      {running ? (
        <div className="stack">
          <div className="row small">
            <span>
              {sc.window === 0 ? "Opening the radio…" : `${sc.forS ? `Pass ${sc.pass + 1}, w` : "W"}indow ${sc.window} of ${sc.windows}`}
              {sc.keepUp != null && <span className="muted"> · keeping up with {Math.round(sc.keepUp * 100)} % of the samples on {sc.cores} core{sc.cores === 1 ? "" : "s"}</span>}
            </span>
            <span className="spacer" />
            <span className="muted">
              {duration(sc.elapsedS)} in · about {duration(sc.leftS)} left
            </span>
          </div>
          <progress className="survey-progress" value={sc.progress} max={1} />
          {sc.behind && <div className="banner warn small">The {sc.behind}</div>}
        </div>
      ) : (
        <div className="banner subtle small">
          {sc.state === "done" ? "Done" : sc.state === "stopped" ? "Stopped" : "Ended"} after {duration(sc.elapsedS)}: {sc.kept} of {sc.sites.length} sites kept, {sc.dropped} dropped.
        </div>
      )}
      {running && spectrum && (
        <Waterfall
          radio={spectrum}
          label="Check sites"
          ccs={sc.freqs
            .filter((f) => Math.abs(f.freqHz - spectrum.centerHz) <= spectrum.rateHz / 2)
            .map((f): CcMark => ({ hz: f.freqHz, label: "", color: MARK_COLOR[f.quality ?? "none"], title: `${formatMhz(f.freqHz, 5)} MHz: ${f.heard ?? "not heard yet"}` }))}
          calls={[]}
        />
      )}
      <div className="row small">
        {(Object.keys(VERDICT_CLASS) as SiteVerdict[])
          .filter((v) => counts[v])
          .map((v) => (
            <span key={v} className={`chip ${VERDICT_CLASS[v]}`}>
              {counts[v]} {v}
            </span>
          ))}
        <span className="spacer" />
        <button className="btn ghost" onClick={() => download(`${stem(sc.name)}-checked.csv`, sc.checkedCsv)} title="The sites file without the inactive, other-site and out-of-range sites">
          Download checked list
        </button>
        <button className="btn ghost" onClick={() => download(`${stem(sc.name)}-report.csv`, sc.reportCsv)} title="What each frequency showed">
          Download report
        </button>
      </div>
      <table className="calls site-check-sites">
        <thead>
          <tr>
            <th>Verdict</th>
            <th>Site</th>
            <th>Signal</th>
            <th>Decode</th>
            <th className="num">Miles</th>
            <th>Why</th>
          </tr>
        </thead>
        <tbody>
          {sc.sites.map((x, i) => (
            <tr key={i} className={`${rowOf(x.quality)} ${x.keep ? "" : "muted"}`}>
              <td>
                <span className={`chip ${VERDICT_CLASS[x.verdict]}`}>{x.verdict}</span>
              </td>
              <td>{x.label}</td>
              <td>
                <Strength db={x.peakDb} q={x.quality} />
              </td>
              <td>
                <Decode q={x.quality} />
              </td>
              <td className="num">{x.miles == null ? "" : Math.round(x.miles)}</td>
              <td className="small">{x.why}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <details className="help" open={freqsOpen} onToggle={(e) => setFreqsOpen((e.target as HTMLDetailsElement).open)}>
        <summary>What each frequency showed ({sc.freqs.length})</summary>
        <table className="calls">
          <thead>
            <tr>
              <th className="num">MHz</th>
              <th className="num">Up</th>
              <th>Signal</th>
              <th>Decode</th>
              <th className="num">Listened</th>
              <th>Heard</th>
            </tr>
          </thead>
          <tbody>
            {sc.freqs.map((f) => (
              <tr key={f.freqHz} className={f.heard == null ? "muted" : rowOf(f.quality)}>
                <td className="num mono">{formatMhz(f.freqHz, 5)}</td>
                <td className="num">{f.heard == null ? "" : pct(f.on)}</td>
                <td>{f.heard == null ? null : <Strength db={f.peakDb} q={f.quality} />}</td>
                <td>
                  <Decode q={f.quality} share={f.decodedShare} />
                </td>
                <td className="num">{f.listenedS > 0 ? duration(f.listenedS) : ""}</td>
                <td className="small">{f.heard ?? "not yet"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </details>
      {sc.savedTo.length > 0 && (
        <p className="muted small">
          Also saved on the recorder's computer: <span className="mono">{sc.savedTo.join(", ")}</span>
        </p>
      )}
      <p className="muted small">
        Rows are coloured by how well they decoded: <span className="q-good-text">green</span> a clean decode on a good signal (12 dB or more),{" "}
        <span className="q-marginal-text">yellow</span> marginal (it decodes, but with errors, weakly, or without control messages),{" "}
        <span className="q-poor-text">red</span> poor (nothing decoded, or most of it failed). <b>Signal</b> is the strongest peak, in dB over the noise floor. <b>Unconfirmed</b> sites are kept: something is on their frequency, but it didn't decode as this system (too weak, a neighbour, or a frequency other sites share). A DMR site judged
        inactive was only quiet while it was listened to — a longer run at a busier time gives it more chances.
      </p>
    </div>
  );
}
