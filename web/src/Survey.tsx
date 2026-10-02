// "Find my system": the survey. Scan the bands for P25, SmartNet and trunked DMR control channels,
// listen to the best one, and add it as a system — control channels, site
// identity, the dongle's frequency correction, gain and centre from what it
// announces. Neighbouring sites it announces can be added as systems too.
// SmartNet doesn't announce its band plan: listening learns it from which
// carrier comes up when a channel number is granted.
// The recorder does the work (crates/trunk-core/src/survey.rs); this shows it.

import { useState } from "react";
import { formatMhz, systemWithChannel } from "./config.ts";
import { addDmrSite, addSite, applySurvey, setNotice, startSurvey, stopSurvey, surveyListen, surveyRescan, useApp, web } from "./controller.ts";
import type { Config, SiteIdentity, SurveyCandidate, SurveyIdentity, SurveyMonitor, SurveySuggestion } from "./protocol.ts";
import { Waterfall } from "./Waterfall.tsx";

const hex = (v: number | null | undefined) => (v === null || v === undefined ? "?" : v.toString(16).toUpperCase());

function idText(id: SurveyIdentity): string {
  const parts: string[] = [];
  if (id.nac !== null) parts.push(`NAC ${hex(id.nac)}`);
  if (id.sysId !== null) parts.push(`SysID ${hex(id.sysId)}`);
  if (id.rfss !== null && id.site !== null) parts.push(`site ${id.rfss}-${id.site}`);
  return parts.join(" · ");
}

const KIND: Record<SurveyCandidate["kind"], string> = {
  control: "Control channel",
  smartnet: "SmartNet control channel",
  dmrControl: "DMR control / rest channel",
  p25: "P25 (voice / data)",
  dmr: "DMR (conventional / data)",
  other: "Not P25, SmartNet or DMR",
};
const isControl = (c: SurveyCandidate) => c.kind === "control" || c.kind === "smartnet";

/** The site key of a scanned control channel: its secondaries share it. */
const siteKey = (id: SurveyIdentity) => (id.sysId !== null && id.site !== null ? `${id.nac}/${id.sysId}/${id.rfss}/${id.site}` : null);

function Candidates(props: { c: Config; list: SurveyCandidate[]; listening: number | null; onListen?: (hz: number) => void }) {
  // Add a scanned control channel as a system — with its site's other control channels found in the scan.
  const add = (cand: SurveyCandidate) => {
    const key = siteKey(cand.identity);
    const same = props.list.filter((x) => x.kind === "control" && (x === cand || (key !== null && siteKey(x.identity) === key)));
    // (On the 6.25 kHz channel raster.)
    const ccs = same.map((x) => Math.round((x.correctedHz ?? x.freqHz) / 6250) * 6250);
    const id = cand.identity;
    const expect: SiteIdentity = { nac: id.nac, sysId: id.sysId, rfss: id.rfss, site: id.site, wacn: id.wacn };
    const name = addSite(ccs, expect);
    setNotice(`Added ${name}: control channel${ccs.length === 1 ? "" : "s"} ${ccs.map((f) => formatMhz(f)).join(", ")} MHz. Listening to it fills in the rest (frequency correction, voice channels).`);
  };
  const addDmr = (cand: SurveyCandidate) => {
    const hz = Math.round((cand.correctedHz ?? cand.freqHz) / 6250) * 6250;
    const cc = cand.dmr?.colorCode ?? null;
    const name = addDmrSite(hz, cc);
    const plus = cand.dmr?.variant === "DMR Capacity Plus";
    setNotice(
      `Added ${name}: ${cand.dmr?.variant ?? "DMR"} on ${formatMhz(hz)} MHz${cc === null ? "" : `, colour code ${cc}`}. ` +
        (plus
          ? "Capacity Plus: add the site's other repeaters under Site frequencies — only the rest channel shows in a scan."
          : "Add the site's voice frequencies under Voice frequencies; which channel is which is learned from the air."),
    );
  };
  const [showOther, setShowOther] = useState(false);
  const others = props.list.filter((c) => c.kind === "other").length;
  const rows = props.list.filter((c) => showOther || c.kind !== "other");
  const decoded = (c: SurveyCandidate) => (c.good + c.bad ? `${Math.round((100 * c.good) / (c.good + c.bad))} %` : "");
  return (
    <div className="stack">
      {rows.length ? (
        <div className="table-wrap">
          <table className="calls">
            <thead>
              <tr>
                <th>MHz</th>
                <th>Signal</th>
                <th>What</th>
                <th>System</th>
                <th>Decoded</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {rows.map((c) => (
                <tr key={c.freqHz} className={isControl(c) ? "" : "st-monitoring"}>
                  <td className="mono" title={c.correctedHz ? `heard at ${formatMhz(c.freqHz)} MHz before correction` : "as heard, before frequency correction"}>
                    {formatMhz(c.correctedHz ?? c.freqHz)}
                  </td>
                  <td className="mono">{c.snrDb.toFixed(0)} dB</td>
                  <td>
                    {KIND[c.kind]}
                    {c.modulation && <span className="tag">{c.modulation}</span>}
                  </td>
                  <td className="mono small">
                    {c.dmr ? [c.dmr.variant?.replace(/^DMR /, ""), c.dmr.colorCode !== null ? `CC ${c.dmr.colorCode}` : null].filter(Boolean).join(" · ") : idText(c.identity)}
                  </td>
                  <td className="mono small">{decoded(c)}</td>
                  <td className="actions">
                    {c.kind === "control" &&
                      (() => {
                        const have = systemWithChannel(props.c, c.correctedHz ?? c.freqHz);
                        return have ? (
                          <span className="muted small">in {have.shortName}</span>
                        ) : (
                          <button className="btn ghost small" onClick={() => add(c)} title="Add this control channel (and its site's others) as a system">
                            Add
                          </button>
                        );
                      })()}
                    {c.kind === "dmrControl" &&
                      (() => {
                        const have = systemWithChannel(props.c, c.freqHz);
                        return have ? (
                          <span className="muted small">in {have.shortName}</span>
                        ) : (
                          <button className="btn ghost small" onClick={() => addDmr(c)} title="Add this DMR site as a system">
                            Add
                          </button>
                        );
                      })()}
                    {c.kind === "smartnet" && !props.onListen && <span className="muted small">listen to learn its band plan</span>}
                    {isControl(c) && props.onListen && (
                      <button className="btn ghost small" disabled={props.listening !== null && Math.abs(props.listening - c.freqHz) < 6000} onClick={() => props.onListen?.(c.freqHz)}>
                        {props.listening !== null && Math.abs(props.listening - c.freqHz) < 6000 ? "Listening" : "Listen"}
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <p className="empty small">Nothing found yet.</p>
      )}
      {others > 0 && (
        <label className="toggle small">
          <input type="checkbox" checked={showOther} onChange={(e) => setShowOther(e.target.checked)} />
          <span>
            Show {others} other continuous signal{others === 1 ? "" : "s"} (not P25, SmartNet or DMR: other trunking systems, data)
          </span>
        </label>
      )}
    </div>
  );
}

function Check(props: { state: "ok" | "wait" | "info"; label: string; children?: React.ReactNode }) {
  return (
    <li className={`check check-${props.state}`}>
      <span className="check-mark" aria-hidden="true">
        {props.state === "ok" ? "✓" : props.state === "wait" ? "…" : "·"}
      </span>
      <span>
        <b>{props.label}</b> {props.children}
      </span>
    </li>
  );
}

const mhzList = (l: number[]) => l.map((f) => formatMhz(f)).join(", ");

/** What the SmartNet band plan search has found so far. */
function SmartnetPlan(props: { s: NonNullable<SurveyMonitor["smartnet"]> }) {
  const { s } = props;
  const found = `${s.points.length} of ${s.channels.length} granted channel${s.channels.length === 1 ? "" : "s"} located`;
  if (!s.bandplan) {
    return (
      <>
        learning it from where granted channels come up — {found}
        {s.ccChan !== null ? ` (this is channel ${s.ccChan})` : ""}. It needs two channels on one line; calls on channels this source can see make it quicker.
      </>
    );
  }
  const b = s.bandplan;
  return (
    <>
      <span className="mono">
        {b.bandplan === "400_custom"
          ? `${b.bandplan}: channel ${b.bandplanOffset} = ${formatMhz(b.bandplanBase ?? 0)} MHz, ${(b.bandplanSpacing ?? 0) / 1000} kHz steps`
          : b.bandplan}
      </span>{" "}
      — learned from the air ({found}, {s.inliers ?? 0} on the line)
    </>
  );
}

function MonitorView(props: { m: SurveyMonitor; sug: SurveySuggestion | null; c: Config; source: number; onDone: () => void }) {
  const { m, sug, c } = props;
  const total = m.good + m.bad;
  const pct = total ? Math.round((100 * m.good) / total) : 0;
  const src = c.sources[props.source];
  const rtl = src?.kind === "rtlsdr";
  const id = m.identity;
  const gainSteps = m.gain.steps.length;
  // The system this is already (one of its control channels), else a new one.
  const existing = sug ? c.systems.findIndex((x) => sug.controlChannels.some((f) => x.controlChannels.some((g) => Math.abs(f - g) < 6_000))) : -1;
  const [picked, setTarget] = useState<number | "new" | null>(null);
  const target = picked ?? (existing >= 0 ? existing : "new");
  const apply = () => {
    if (!sug) return;
    const done = applySurvey(props.source, sug, target);
    stopSurvey();
    const bits = [`control channel${sug.controlChannels.length === 1 ? "" : "s"} ${mhzList(sug.controlChannels)} MHz`];
    if (sug.type === "smartnet" && sug.bandplan) bits.unshift(`SmartNet, band plan ${sug.bandplan.bandplan}`);
    if (sug.site !== null) bits.push(`site lock ${sug.rfss ?? "?"}-${sug.site}`);
    if (src && src.kind !== "file") {
      if (sug.ppmApply !== null) bits.push(`correction ${sug.ppmApply > 0 ? "+" : ""}${sug.ppmApply} ppm`);
      if (rtl && sug.gainDb !== null) bits.push(`gain ${sug.gainDb} dB`);
      bits.push(done.centered ? `center ${formatMhz(sug.centerHz, 4)} MHz` : `source ${props.source + 1} left where it is (another system needs it) — give this system a source centered at ${formatMhz(sug.centerHz, 4)} MHz`);
    }
    setNotice(`${target === "new" ? "Added" : "Updated"} ${done.system}: ${bits.join(", ")}. Press Start to record.`);
    props.onDone();
  };
  const neighbour = (a: SurveyMonitor["adjacent"][number]) => {
    const name = addSite([a.freqHz], { wacn: id.wacn, sysId: a.sysId, rfss: a.rfss, site: a.site });
    setNotice(`Added ${name} (site ${a.rfss}-${a.site}, control channel ${formatMhz(a.freqHz)} MHz). It needs a source that covers it.`);
  };
  return (
    <div className="stack">
      <ul className="checklist">
        <Check state={m.good >= 10 ? "ok" : "wait"} label="Control channel">
          {total ? (
            <>
              decoding {pct} % of {total} messages{m.modulation ? ` · ${m.modulation}` : ""}
              {m.snrDb !== null ? ` · ${m.snrDb.toFixed(0)} dB above the noise` : ""}
            </>
          ) : (
            "waiting for messages…"
          )}
        </Check>
        {m.smartnet ? (
          <Check state={id.sysId !== null ? "ok" : "wait"} label="System">
            {id.sysId !== null ? <span className="mono">SmartNet · System ID {hex(id.sysId)}</span> : "SmartNet — waiting for the system ID broadcast…"}
          </Check>
        ) : (
        <Check state={id.wacn !== null && id.sysId !== null ? "ok" : "wait"} label="System">
          {id.wacn !== null || id.sysId !== null || id.nac !== null ? (
            <span className="mono">
              WACN {hex(id.wacn)} · SysID {hex(id.sysId)} · NAC {hex(id.nac)}
              {id.rfss !== null && id.site !== null ? ` · RFSS ${id.rfss} site ${id.site}` : ""}
            </span>
          ) : (
            "waiting for the network status broadcast…"
          )}
        </Check>
        )}
        {m.smartnet ? (
          <Check state={m.smartnet.bandplan ? "ok" : "wait"} label="Band plan">
            <SmartnetPlan s={m.smartnet} />
          </Check>
        ) : (
          <Check state={m.idens > 0 ? "ok" : "wait"} label="Band plan">
            {m.idens > 0 ? `${m.idens} channel table${m.idens === 1 ? "" : "s"} (IDEN) heard` : "waiting for the channel tables…"}
          </Check>
        )}
        <Check state={m.ppm !== null ? "ok" : "wait"} label="Frequency correction">
          {m.ppm !== null && m.advertisedHz !== null && m.offsetHz !== null ? (
            <>
              the channel announces {formatMhz(m.advertisedHz)} MHz and is heard {Math.abs(m.offsetHz).toFixed(0)} Hz {m.offsetHz < 0 ? "low" : "high"} →{" "}
              <b className="mono">
                {m.ppm > 0 ? "+" : ""}
                {m.ppm.toFixed(2)} ppm
              </b>
              {rtl ? ` (the dongle takes ${Math.round(m.ppm)})` : ""}
            </>
          ) : m.advertisedHz === null ? (
            "waiting for the channel to announce its frequency…"
          ) : (
            "measuring…"
          )}
        </Check>
        {m.gain.state !== "off" && (
          <Check state={m.gain.state === "done" ? "ok" : "wait"} label="Gain">
            {m.gain.state === "done"
              ? m.gain.bestDb !== null
                ? `${m.gain.bestDb} dB is the lowest with the best signal (tried ${gainSteps})`
                : "couldn't pick one; keeping the current setting"
              : m.gain.state === "running"
                ? `trying settings… (${gainSteps} done)`
                : "tried once decoding is steady"}
          </Check>
        )}
        <Check state="info" label="Alternate control channels">
          {m.secondary.length ? <span className="mono">{mhzList(m.secondary)} MHz</span> : "none announced (yet)"}
        </Check>
        <Check state="info" label="Neighbouring sites">
          {m.adjacent.length ? (
            <span className="row small">
              {m.adjacent.map((a) => {
                const have = systemWithChannel(c, a.freqHz);
                return (
                  <span key={`${a.rfss}-${a.site}`} className="chip">
                    <span className="mono">
                      site {a.rfss}-{a.site} · {formatMhz(a.freqHz)}
                    </span>
                    {have ? (
                      <span>· in {have.shortName}</span>
                    ) : (
                      <button className="btn ghost small" onClick={() => neighbour(a)} title="Record this site too (as its own system)">
                        Add
                      </button>
                    )}
                  </span>
                );
              })}
            </span>
          ) : (
            "none announced (yet)"
          )}
        </Check>
        <Check state="info" label="Voice channels in use">
          {m.voice.length ? (
            <>
              {m.voice.length} seen
              {m.voice.some((v) => v.tdma) ? " (Phase 2 TDMA among them)" : ""}:{" "}
              <span className="mono small">
                {m.voice
                  .slice(0, 24)
                  .map((v) => formatMhz(v.freqHz, 4))
                  .join(", ")}
                {m.voice.length > 24 ? " …" : ""}
              </span>
            </>
          ) : (
            "none yet — listening longer shows where calls go"
          )}
        </Check>
      </ul>
      {sug && (
        <div className="banner">
          <div className="stack">
            <div>
              <b>Ready to record.</b> This {target === "new" ? "adds a system with" : `sets ${c.systems[target]?.shortName ?? "the system"}'s`} control channel
              {sug.controlChannels.length === 1 ? "" : "s"} <span className="mono">{mhzList(sug.controlChannels)}</span> MHz
              {sug.site !== null && (
                <>
                  , locked to site{" "}
                  <span className="mono">
                    {sug.rfss ?? "?"}-{sug.site}
                  </span>
                </>
              )}
              {src && src.kind !== "file" && (
                <>
                  {sug.ppmApply !== null && (
                    <>
                      , source {props.source + 1}'s correction to{" "}
                      <span className="mono">
                        {sug.ppmApply > 0 ? "+" : ""}
                        {sug.ppmApply}
                      </span>{" "}
                      ppm
                    </>
                  )}
                  {rtl && sug.gainDb !== null && <>, gain to {sug.gainDb} dB</>}
                  , and its center to <span className="mono">{formatMhz(sug.centerHz, 4)}</span> MHz
                </>
              )}
              .
            </div>
            {src && src.kind !== "file" && (
              <div className="small muted">
                {sug.voiceTotal === 0
                  ? "No calls seen yet, so the center is placed around the control channel. Listen a little longer to see where the voice channels are."
                  : sug.voiceCovered < sug.voiceTotal
                    ? `This source covers ${sug.voiceCovered} of the ${sug.voiceTotal} voice channels seen; the system spans ${(sug.spanHz / 1e6).toFixed(1)} MHz — add another dongle (or use a wider SDR) to record the rest.`
                    : `This source covers all ${sug.voiceTotal} voice channels seen so far.`}
              </div>
            )}
            {!m.ready && <div className="small muted">Still measuring — waiting a few more seconds gives a better result.</div>}
            <div className="row">
              {c.systems.length > 0 && (
                <select value={String(target)} onChange={(e) => setTarget(e.target.value === "new" ? "new" : Number(e.target.value))} aria-label="Add as, or replace">
                  <option value="new">as a new system</option>
                  {c.systems.map((x, k) => (
                    <option key={k} value={k}>
                      replacing {x.shortName}
                    </option>
                  ))}
                </select>
              )}
              <button className="btn primary" onClick={apply}>
                {target === "new" ? "Add this system" : "Update it"}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

export function SurveyPanel(props: { c: Config }) {
  const s = useApp();
  const { c } = props;
  const sv = s.survey;
  const recording = s.phase !== "idle";
  const [open, setOpen] = useState(() => c.systems.length === 0);
  const [source, setSource] = useState(0);
  const [picked, setPicked] = useState<string[] | null>(null);
  const [findGain, setFindGain] = useState(true);
  const bands = picked ?? s.surveyBands.filter((b) => b.defaultOn).map((b) => b.id);
  const src = c.sources[Math.min(source, c.sources.length - 1)];
  const active = sv.stage !== "idle";
  const unsupported = web && (src?.kind === "usrp" || src?.kind === "airspy" || src?.kind === "soapy");
  const bandLabel = (id: string) => s.surveyBands.find((b) => b.id === id)?.label ?? id;

  if (!s.surveyBands.length) return null;
  return (
    <section className="panel survey">
      <header className="panel-head">
        <h2>Find my system</h2>
        <div className="row">
          {active ? (
            <>
              {sv.stage !== "scanning" && (
                <button className="btn ghost" onClick={surveyRescan}>
                  Scan again
                </button>
              )}
              <button className="btn ghost" onClick={stopSurvey}>
                {sv.stage === "done" ? "Close" : "Stop"}
              </button>
            </>
          ) : (
            <button className="btn ghost" onClick={() => setOpen(!open)} aria-expanded={open}>
              {open ? "Hide" : "Show"}
            </button>
          )}
        </div>
      </header>
      {!active && open && (
        <div className="stack">
          <p className="muted small">
            Don't know the frequencies? Connect your radio and the recorder scans the public-safety bands for P25 control channels, listens to the strongest, and
            works out the system's control channels, site and your radio's frequency correction — usually in a minute or two. Run it again to add more systems or
            sites; you can still type everything in by hand below. Trunked DMR sites (Capacity Plus, Capacity Max, Connect Plus, Tier III) show up too, with
            their colour code — add the business bands to look where most of them are.
          </p>
          {(c.sources.length > 1 || src?.kind === "rtlsdr") && (
          <div className="grid2">
            {c.sources.length > 1 && (
              <label className="field">
                <span className="field-label">Radio</span>
                <select value={source} onChange={(e) => setSource(Number(e.target.value))}>
                  {c.sources.map((x, i) => (
                    <option key={i} value={i}>
                      Source {i + 1} ({x.kind === "rtlsdr" ? "RTL-SDR" : x.kind === "usrp" ? "USRP" : x.kind === "airspy" ? "Airspy" : x.kind === "soapy" ? "SoapySDR" : "capture file"})
                    </option>
                  ))}
                </select>
              </label>
            )}
            {src?.kind === "rtlsdr" && (
              <label className="toggle">
                <input type="checkbox" checked={findGain} onChange={(e) => setFindGain(e.target.checked)} />
                <span>
                  Find the best gain <span className="field-hint">— adds ~15 s</span>
                </span>
              </label>
            )}
          </div>
          )}
          {src?.kind === "file" ? (
            <p className="muted small">A capture file is examined at its center frequency ({src.centerHz ? `${formatMhz(src.centerHz, 4)} MHz` : "set it below"}).</p>
          ) : (
            <fieldset className="bands">
              <legend className="field-label">Bands to scan</legend>
              {s.surveyBands.map((b) => (
                <label key={b.id} className="toggle">
                  <input
                    type="checkbox"
                    checked={bands.includes(b.id)}
                    onChange={(e) => setPicked(e.target.checked ? [...bands, b.id] : bands.filter((x) => x !== b.id))}
                  />
                  <span>
                    {b.label} <span className="field-hint mono">{`${(b.loHz / 1e6).toFixed(0)}–${(b.hiHz / 1e6).toFixed(0)}`}</span>
                  </span>
                </label>
              ))}
            </fieldset>
          )}
          {unsupported && <p className="warn small">USRP, Airspy and SoapySDR need the desktop app.</p>}
          <div className="row">
            <button
              className="btn primary"
              disabled={!s.connected || recording || unsupported || (src?.kind !== "file" && !bands.length)}
              title={recording ? "Stop recording first" : ""}
              onClick={() => startSurvey(Math.min(source, c.sources.length - 1), bands, src?.kind === "rtlsdr" && findGain)}
            >
              Scan
            </button>
            {recording && <span className="muted small">Stop recording to scan — the scan needs the radio to itself.</span>}
          </div>
        </div>
      )}
      {sv.stage !== "idle" && (
        <div className="stack">
          {sv.error && <div className="banner bad small">{sv.error}</div>}
          {sv.stage === "scanning" && sv.progress && (
            <div className="stack">
              <div className="row small">
                <span>
                  Scanning <b>{bandLabel(sv.progress.band)}</b> around <span className="mono">{formatMhz(sv.progress.centerHz, 3)}</span> MHz
                </span>
                <span className="spacer" />
                <span className="muted">
                  step {sv.progress.hop} of {sv.progress.hops}
                </span>
              </div>
              <progress className="survey-progress" value={sv.progress.hop - 1} max={sv.progress.hops} />
            </div>
          )}
          {sv.monitor && (
            <>
              <div className="row">
                <span>
                  {sv.stage === "monitoring" ? "Listening to" : "Listened to"} <span className="mono">{formatMhz(sv.monitor.advertisedHz ?? sv.monitor.heardHz)}</span> MHz
                  {sv.stage === "monitoring" && <span className="muted small"> · {sv.monitor.elapsedS.toFixed(0)} s</span>}
                </span>
              </div>
              {sv.stage === "monitoring" && s.surveySpectrum && <Waterfall radio={s.surveySpectrum} label="Survey" ccs={[{ hz: sv.monitor.heardHz, label: "CC", color: "var(--accent)" }]} calls={[]} />}
              <MonitorView m={sv.monitor} sug={sv.suggest} c={c} source={sv.source} onDone={() => setOpen(false)} />
            </>
          )}
          {sv.message && <div className="banner subtle">{sv.message}</div>}
          {(sv.candidates.length > 0 || !sv.monitor) && (
            <details className="help" open={!sv.monitor}>
              <summary>
                Signals found ({sv.candidates.filter(isControl).length} control channel
                {sv.candidates.filter(isControl).length === 1 ? "" : "s"})
              </summary>
              <Candidates c={c} list={sv.candidates} listening={sv.monitor?.freqHz ?? null} onListen={surveyListen} />
            </details>
          )}
        </div>
      )}
    </section>
  );
}
