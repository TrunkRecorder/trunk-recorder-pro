// "Find my system": the survey. Scan the bands for P25, SmartNet, trunked DMR and NXDN control channels,
// listen to the best one, and add it as a system — control channels, site
// identity, the dongle's frequency correction, gain and centre from what it
// announces. Neighbouring sites it announces can be added as sites of it too.
// SmartNet doesn't announce its band plan: listening learns it from which
// carrier comes up when a channel number is granted.
// Looking for a system (lookFor.ts: one from the directory, known only by its
// licenses or by its sites, a sites file, or one set up already) it scans just
// its frequencies: the signals on its licenses are marked, once the radio's
// error is known it listens to one of its control channels, and each of its
// sites is judged by what its frequencies showed, to keep the ones heard
// here. A spare radio (one no source uses) scans beside the recording.
// The recorder does the work (crates/trunk-core/src/survey.rs); this shows it.

import { Fragment, useEffect, useRef, useState } from "react";
import { adjacentIdentity, deviceId, fccKey, fccPrimary, formatGain, formatMhz, hasBiasT, needsDriver, siteNameOf, SOURCE_KINDS, sourceKey, sourceTitle, roaming, siteAddress, systemWithChannel } from "./config.ts";
import {
  can,
  addDmrSite,
  addNxdnChannel,
  addNxdnSite,
  addSite,
  applySurvey,
  bandsHolding,
  licenseeName,
  lookFor,
  refreshDevices,
  setNotice,
  spareRadios,
  startSurvey,
  stopSurvey,
  surveyListen,
  surveyRescan,
  systemOfSite,
  updateConfig,
  useApp,
  type SurveyTarget,
  holdsRadios,
} from "./controller.ts";
import { findExisting, openImport, toFind } from "./directoryImport.tsx";
import { defaultListenS, lookedFreqs, type LookingFor } from "./lookFor.ts";
import type { Config, FccLicense, FccLicenses, SiteIdentity, Source, SurveyCandidate, SurveyMonitor, SurveySite, SurveySuggestion } from "./protocol.ts";
import type { LicenseLookup } from "./controller.ts";
import { IdentifyBar, LicenseCell, licensesOn } from "./Identify.tsx";
import { BiasT } from "./inputs.tsx";
import { Fig, Figs, Info } from "./charts.tsx";
import { Waterfall } from "./Waterfall.tsx";

const hex = (v: number | null | undefined) => (v === null || v === undefined ? "?" : v.toString(16).toUpperCase());

function idText(id: SiteIdentity): string {
  const parts: string[] = [];
  if (id.nac != null) parts.push(`NAC ${hex(id.nac)}`);
  if (id.sysId != null) parts.push(`SysID ${hex(id.sysId)}`);
  if (id.rfss != null && id.site != null) parts.push(`site ${id.rfss}-${id.site}`);
  return parts.join(" · ");
}

const KIND: Record<SurveyCandidate["kind"], string> = {
  control: "Control channel",
  smartnet: "SmartNet control channel",
  edacs: "EDACS control channel",
  dmrControl: "DMR control / rest channel",
  p25: "P25 (voice / data)",
  dmr: "DMR (conventional / data)",
  nxdnControl: "NXDN control channel",
  nxdn: "NXDN (conventional / voice)",
  other: "Other",
};
const isControl = (c: SurveyCandidate) => c.kind === "control" || c.kind === "smartnet";
/** As heard, corrected once the radio's error is known. */
const heardHz = (c: SurveyCandidate) => c.correctedHz ?? c.freqHz;

/** The license on `hz` of the system looked for (its licenses: `sought`), or undefined. */
const soughtOn = (sought: FccLicenses | null, hz: number | null | undefined) => (sought && hz != null ? sought[fccKey(hz)] : undefined);

/** A signal on the licenses of the system looked for: its licensee and call sign. */
function Sought(props: { l: FccLicense }) {
  return (
    <span className="tag sought" title="On the licenses of the system you're looking for">
      {props.l.name ? `${licenseeName(props.l.name)} · ` : ""}
      <span className="mono">{props.l.callSign}</span>
    </span>
  );
}

/** The site key of a scanned control channel: its secondaries share it. */
const siteKey = (id: SiteIdentity) => (id.sysId != null && id.site != null ? `${id.nac}/${id.sysId}/${id.rfss}/${id.site}` : null);

function Candidates(props: { c: Config; list: SurveyCandidate[]; listening: number | null; onListen?: (hz: number) => void; licenses: LicenseLookup | null; sought: FccLicenses | null }) {
  // Add a scanned control channel as a system — with its site's other control channels found in the scan.
  const add = (cand: SurveyCandidate) => {
    const key = siteKey(cand.identity);
    const same = props.list.filter((x) => x.kind === "control" && (x === cand || (key !== null && siteKey(x.identity) === key)));
    // (On the 6.25 kHz channel raster.)
    const ccs = same.map((x) => Math.round((x.correctedHz ?? x.freqHz) / 6250) * 6250);
    const id = cand.identity;
    const expect: SiteIdentity = { ...id };
    const name = addSite(ccs, expect);
    setNotice(`Added ${name}: control channel${ccs.length === 1 ? "" : "s"} ${ccs.map((f) => formatMhz(f)).join(", ")} MHz.`);
  };
  const addDmr = (cand: SurveyCandidate) => {
    const hz = Math.round((cand.correctedHz ?? cand.freqHz) / 6250) * 6250;
    const cc = cand.dmr?.colorCode ?? null;
    const name = addDmrSite(hz, cc);
    const plus = cand.dmr?.variant === "DMR Capacity Plus";
    setNotice(
      `Added ${name}: ${cand.dmr?.variant ?? "DMR"} on ${formatMhz(hz)} MHz${cc === null ? "" : `, colour code ${cc}`}. ` +
        (plus
          ? "Add the other repeaters under Site frequencies."
          : "Add voice channels under Voice frequencies."),
    );
  };
  const addNxdn = (cand: SurveyCandidate) => {
    const hz = Math.round((cand.correctedHz ?? cand.freqHz) / 6250) * 6250;
    const rate = cand.nxdn?.rate ?? "nxdn48";
    const ran = cand.nxdn?.ran ?? null;
    if (cand.kind === "nxdnControl") {
      const name = addNxdnSite(hz, rate, ran);
      setNotice(
        `Added ${name}: NXDN Type-C on ${formatMhz(hz)} MHz${ran === null ? "" : `, RAN ${ran}`}. ` +
          "Add its channel table, or its voice frequencies so channel numbers are learned.",
      );
    } else {
      const name = addNxdnChannel(hz, rate, ran);
      setNotice(`Added ${formatMhz(hz)} MHz (${rate.toUpperCase()}${ran ? `, RAN ${ran}` : ""}) to ${name}'s conventional channels.`);
    }
  };
  const [showOther, setShowOther] = useState(false);
  const others = props.list.filter((c) => c.kind === "other").length;
  const sought = (c: SurveyCandidate) => soughtOn(props.sought, heardHz(c));
  // (The system looked for: its signals first, even ones not decoded.)
  const rows = props.list.filter((c) => showOther || c.kind !== "other" || sought(c)).sort((a, b) => Number(!!sought(b)) - Number(!!sought(a)));
  const looked = props.licenses?.stage === "done" || !!props.sought;
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
                {looked && <th>Licensed to</th>}
                <th />
              </tr>
            </thead>
            <tbody>
              {rows.map((c) => (
                <tr key={c.freqHz} className={`${isControl(c) ? "" : "st-monitoring"}${sought(c) ? " sought" : ""}`}>
                  <td className="mono" title={c.correctedHz ? `${formatMhz(c.freqHz)} MHz uncorrected` : "Uncorrected"}>
                    {formatMhz(c.correctedHz ?? c.freqHz)}
                  </td>
                  <td className="mono">{c.snrDb.toFixed(0)} dB</td>
                  <td>
                    {KIND[c.kind]}
                    {c.modulation && <span className="tag">{c.modulation}</span>}
                  </td>
                  <td className="mono small">
                    {c.dmr
                      ? [c.dmr.variant?.replace(/^DMR /, ""), c.dmr.colorCode !== null ? `CC ${c.dmr.colorCode}` : null].filter(Boolean).join(" · ")
                      : c.nxdn
                        ? [c.nxdn.ran !== null ? `RAN ${c.nxdn.ran}` : null, c.nxdn.system !== null ? `SysID ${hex(c.nxdn.system)}` : null, c.nxdn.site !== null ? `site ${c.nxdn.site}` : null]
                            .filter(Boolean)
                            .join(" · ")
                        : idText(c.identity)}
                  </td>
                  <td className="mono small">{decoded(c)}</td>
                  {looked && (
                    <td>
                      {sought(c) ? <Sought l={sought(c)!} /> : <LicenseCell found={licensesOn(props.licenses, Math.round(heardHz(c) / 6250) * 6250)} />}
                    </td>
                  )}
                  <td className="actions">
                    {c.kind === "control" &&
                      (() => {
                        const have = systemWithChannel(props.c, c.correctedHz ?? c.freqHz);
                        return have ? (
                          <span className="muted small">in {have.shortName}</span>
                        ) : (
                          <button className="btn ghost small" onClick={() => add(c)} title="Includes the site's other control channels">
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
                          <button className="btn ghost small" onClick={() => addDmr(c)}>
                            Add
                          </button>
                        );
                      })()}
                    {(c.kind === "nxdnControl" || c.kind === "nxdn") &&
                      (() => {
                        const hz = c.correctedHz ?? c.freqHz;
                        const have = systemWithChannel(props.c, hz);
                        const conv = props.c.conventional.find((v) => v.channels.some((ch) => Math.abs(ch.freqHz - hz) < 6_000));
                        return have || conv ? (
                          <span className="muted small">in {(have ?? conv)!.shortName}</span>
                        ) : (
                          <button className="btn ghost small" onClick={() => addNxdn(c)} title={c.kind === "nxdn" ? "As a conventional channel" : undefined}>
                            Add
                          </button>
                        );
                      })()}
                    {c.kind === "smartnet" && !props.onListen && <span className="muted small">listen for band plan</span>}
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
            Show {others} other signal{others === 1 ? "" : "s"}
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
  const found = <Fig v={`${s.points.length}/${s.channels.length}`} u="channels located" title="Granted channels located" />;
  if (!s.bandplan) {
    return (
      <>
        learning · {found}
        {s.ccChan !== null && (
          <>
            {" · CC "}
            <Fig v={s.ccChan} title="This control channel's channel number" />
          </>
        )}
      </>
    );
  }
  const b = s.bandplan;
  return (
    <>
      <span className="mono">
        {b.bandplan === "400_custom"
          ? `${b.bandplan}: channel ${b.bandplanOffset} = ${formatMhz(b.bandplanBaseHz ?? 0)} MHz, ${(b.bandplanSpacingHz ?? 0) / 1000} kHz steps`
          : b.bandplan}
      </span>{" "}
      · {found}
    </>
  );
}

function MonitorView(props: { m: SurveyMonitor; sug: SurveySuggestion | null; c: Config; source: number; onDone: () => void; sought: FccLicenses | null }) {
  const { m, sug, c, sought } = props;
  const mine = soughtOn(sought, m.advertisedHz);
  const total = m.good + m.bad;
  const pct = total ? Math.round((100 * m.good) / total) : 0;
  // Of the air, not of the messages: an MBT takes several TSBKs' worth.
  const air = m.airS && m.elapsedS >= 2 ? Math.min(100, Math.round((100 * m.airS) / m.elapsedS)) : null;
  const src = c.sources[props.source];
  const rtl = src?.type === "rtlsdr";
  const id = m.identity;
  const gainSteps = m.gain.steps.length;
  // The site this is already (one of its control channels), else a site of the system it is of, else a new system.
  const near = (f: number, g: number) => Math.abs(f - g) < 6_000;
  const existing = sug
    ? c.systems.flatMap((x, k) => x.sites.map((y, j) => ({ system: k, site: j, y }))).find(({ y }) => sug.controlChannels.some((f) => y.controlChannelsHz.some((g) => near(f, g))))
    : undefined;
  const of = sug ? systemOfSite(c, sug.identity) : -1;
  const [picked, setTarget] = useState<SurveyTarget | null>(null);
  const target: SurveyTarget = picked ?? (existing ? { system: existing.system, site: existing.site } : of >= 0 ? { system: of, site: "new" } : "new");
  const targetSys = target === "new" ? undefined : c.systems[target.system];
  const replacing = target !== "new" && target.site !== "new";
  const encode = (t: SurveyTarget) => (t === "new" ? "new" : `${t.system}:${t.site}`);
  const decode = (v: string): SurveyTarget => {
    if (v === "new") return "new";
    const [k, j] = v.split(":");
    return { system: Number(k), site: j === "new" ? "new" : Number(j) };
  };
  const apply = () => {
    if (!sug) return;
    const done = applySurvey(props.source, sug, target);
    stopSurvey();
    const bits = [`control channel${sug.controlChannels.length === 1 ? "" : "s"} ${mhzList(sug.controlChannels)} MHz`];
    if (sug.type === "smartnet" && sug.bandplan) bits.unshift(`SmartNet, band plan ${sug.bandplan.bandplan}`);
    if (sug.identity.site != null) bits.push(`site lock ${sug.identity.rfss ?? "?"}-${sug.identity.site}`);
    if (src && src.type !== "file") {
      if (sug.ppmApply !== null) bits.push(`correction ${sug.ppmApply > 0 ? "+" : ""}${sug.ppmApply} ppm`);
      if (rtl && sug.gainDb !== null) bits.push(`gain ${formatGain(sug.gainDb)} dB`);
      bits.push(done.centered ? `center ${formatMhz(sug.centerHz, 4)} MHz` : `source ${props.source + 1} unchanged (in use) — needs a source at ${formatMhz(sug.centerHz, 4)} MHz`);
    }
    setNotice(`${replacing ? "Updated" : "Added"} ${done.system}: ${bits.join(", ")}.`);
    props.onDone();
  };
  const neighbour = (a: SurveyMonitor["adjacent"][number]) => {
    const name = addSite([a.freqHz], adjacentIdentity(a));
    setNotice(`Added ${name} (site ${siteAddress(adjacentIdentity(a))}, control channel ${formatMhz(a.freqHz)} MHz). It needs a source that covers it.`);
  };
  return (
    <div className="stack">
      <ul className="checklist">
        <Check state={m.good >= 10 ? "ok" : "wait"} label="Control channel">
          {total ? (
            <Figs
              inline
              items={[
                { k: "Decoded", v: pct, u: "%", level: pct < 50 ? "warn" : undefined, title: `Of ${total} messages` },
                { k: "Messages", v: total },
                air !== null && { k: "Coverage", v: air, u: "%" },
                !!m.modulation && { k: "Mod", v: m.modulation },
                m.snrDb !== null && { k: "SNR", v: m.snrDb.toFixed(0), u: "dB" },
              ]}
            />
          ) : (
            "waiting for messages…"
          )}
        </Check>
        {m.smartnet ? (
          <Check state={id.sysId != null ? "ok" : "wait"} label="System">
            {id.sysId != null ? <Figs inline items={[{ k: "SmartNet System ID", v: hex(id.sysId) }]} /> : "SmartNet · waiting for its System ID…"}
          </Check>
        ) : (
        <Check state={id.wacn != null && id.sysId != null ? "ok" : "wait"} label="System">
          {id.wacn != null || id.sysId != null || id.nac != null ? (
            <Figs
              inline
              items={[
                { k: "WACN", v: hex(id.wacn) },
                { k: "SysID", v: hex(id.sysId) },
                { k: "NAC", v: hex(id.nac) },
                id.rfss != null && id.site != null && { k: "Site", v: `${id.rfss}-${id.site}`, title: `RFSS ${id.rfss} site ${id.site}` },
              ]}
            />
          ) : (
            "waiting for the network status…"
          )}
        </Check>
        )}
        {m.smartnet ? (
          <Check state={m.smartnet.bandplan ? "ok" : "wait"} label="Band plan">
            <SmartnetPlan s={m.smartnet} />
          </Check>
        ) : (
          <Check state={m.idens > 0 ? "ok" : "wait"} label="Band plan">
            {m.idens > 0 ? <Fig v={m.idens} u={m.idens === 1 ? "channel table (IDEN)" : "channel tables (IDEN)"} /> : "waiting for the channel tables…"}
          </Check>
        )}
        <Check state={m.ppm !== null ? "ok" : "wait"} label="Frequency correction">
          {m.ppm !== null && m.advertisedHz !== null && m.offsetHz !== null ? (
            <Figs
              inline
              items={[
                { k: m.offsetHz < 0 ? "Low by" : "High by", v: Math.abs(m.offsetHz).toFixed(0), u: "Hz" },
                { k: "Correction", v: `${m.ppm > 0 ? "+" : ""}${m.ppm.toFixed(2)}`, u: "ppm" },
                rtl && { k: "Dongle", v: Math.round(m.ppm), u: "ppm" },
              ]}
            />
          ) : m.advertisedHz === null ? (
            "waiting for its frequency…"
          ) : (
            "measuring…"
          )}
        </Check>
        {m.gain.state !== "off" && (
          <Check state={m.gain.state === "done" ? "ok" : "wait"} label="Gain">
            {m.gain.state === "done" ? (
              m.gain.bestDb !== null ? (
                <>
                  <Fig v={formatGain(m.gain.bestDb)} u="dB" /> <span className="muted">best of {gainSteps}</span>
                </>
              ) : (
                "no clear best; unchanged"
              )
            ) : m.gain.state === "running" ? (
              <>
                trying… <Fig v={gainSteps} u="done" />
              </>
            ) : (
              "once decoding is steady"
            )}
          </Check>
        )}
        {sought && (
          <Check state={mine ? "ok" : "info"} label="The system you're looking for">
            {mine ? (
              <>
                <span className="chip ok">On its licenses</span> <span className="mono">{mine.callSign}</span>
              </>
            ) : m.advertisedHz === null ? (
              "waiting for this control channel's frequency…"
            ) : (
              <>
                <span className="chip warn">Another system</span> <Info>This control channel isn't on its licenses. Listen to a signal marked as its own, if the scan found one.</Info>
              </>
            )}
          </Check>
        )}
        <Check state="info" label="Alternate control channels">
          {m.secondary.length ? (
            <span className="mono">
              {m.secondary.map((hz, i) => (
                <Fragment key={hz}>
                  {i > 0 && ", "}
                  {formatMhz(hz)}
                  {soughtOn(sought, hz) && <Sought l={soughtOn(sought, hz)!} />}
                </Fragment>
              ))}{" "}
              MHz
            </span>
          ) : (
            "none announced (yet)"
          )}
        </Check>
        <Check state="info" label="Neighbouring sites">
          {m.adjacent.length ? (
            <span className="row small">
              {m.adjacent.map((a) => {
                const have = systemWithChannel(c, a.freqHz);
                return (
                  <span key={siteAddress(adjacentIdentity(a))} className="chip">
                    <span className="mono">
                      {siteAddress(adjacentIdentity(a))} · {formatMhz(a.freqHz)}
                    </span>
                    {roaming(id, adjacentIdentity(a)) !== "intra-system" && <span className="badge">{roaming(id, adjacentIdentity(a))}</span>}
                    {soughtOn(sought, a.freqHz) && <Sought l={soughtOn(sought, a.freqHz)!} />}
                    {have ? (
                      <span>· in {have.shortName}</span>
                    ) : (
                      <button className="btn ghost small" onClick={() => neighbour(a)} title="Add as a site of its system (a new system if it isn't set up)">
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
              <Fig v={m.voice.length} u="seen" /> {m.voice.some((v) => v.tdma) && <span className="chip" title="Phase 2 TDMA among them">TDMA</span>}{" "}
              <span className="mono small">
                {m.voice
                  .slice(0, 24)
                  .map((v) => formatMhz(v.freqHz, 4))
                  .join(", ")}
                {m.voice.length > 24 ? " …" : ""}
              </span>
            </>
          ) : (
            "none yet"
          )}
        </Check>
      </ul>
      {sug && (
        <div className="banner">
          <div className="stack">
            <div>
              <b>Ready to record</b>
              {" · "}
              {target === "new"
                ? "a new system"
                : replacing
                  ? `updates ${targetSys && targetSys.sites.length > 1 ? `${targetSys.shortName}'s site ${siteNameOf(targetSys, targetSys.sites[target.site as number])}` : (targetSys?.shortName ?? "the system")}`
                  : `a new site of ${targetSys?.shortName ?? "the system"}`}
            </div>
            <Figs
              items={[
                { k: sug.controlChannels.length === 1 ? "Control channel" : "Control channels", v: mhzList(sug.controlChannels), u: "MHz" },
                sug.identity.site != null && { k: "Site lock", v: `${sug.identity.rfss ?? "?"}-${sug.identity.site}` },
                !!src && src.type !== "file" && sug.ppmApply !== null && { k: `Source ${props.source + 1} correction`, v: `${sug.ppmApply > 0 ? "+" : ""}${sug.ppmApply}`, u: "ppm" },
                !!src && src.type !== "file" && rtl && sug.gainDb !== null && { k: "Gain", v: formatGain(sug.gainDb), u: "dB" },
                !!src && src.type !== "file" && { k: "Center", v: formatMhz(sug.centerHz, 4), u: "MHz" },
                !!src &&
                  src.type !== "file" &&
                  sug.voiceTotal > 0 && {
                    k: "Voice covered",
                    v: `${sug.voiceCovered}/${sug.voiceTotal}`,
                    level: sug.voiceCovered < sug.voiceTotal ? "warn" : undefined,
                    title: "Voice channels seen inside the source's band",
                  },
                !!src && src.type !== "file" && sug.voiceTotal > 0 && sug.voiceCovered < sug.voiceTotal && { k: "System spans", v: (sug.spanHz / 1e6).toFixed(1), u: "MHz" },
              ]}
            />
            {src && src.type !== "file" && sug.voiceTotal === 0 && <div className="small muted">No calls yet: centered on the control channel</div>}
            {!m.ready && <div className="small muted">Still measuring…</div>}
            <div className="row">
              {c.systems.length > 0 && (
                <select value={encode(target)} onChange={(e) => setTarget(decode(e.target.value))} aria-label="Add as, or replace">
                  <option value="new">as a new system</option>
                  {c.systems.map((x, k) => (
                    <Fragment key={k}>
                      {x.type === (sug.type === "smartnet" ? "smartnet" : "p25") && <option value={`${k}:new`}>as a site of {x.shortName}</option>}
                      {x.sites.map((y, j) => (
                        <option key={j} value={`${k}:${j}`}>
                          replacing {x.sites.length > 1 ? `${x.shortName}'s site ${siteNameOf(x, y)}` : x.shortName}
                        </option>
                      ))}
                    </Fragment>
                  ))}
                </select>
              )}
              <button className="btn primary" onClick={apply}>
                {target === "new" ? "Add this system" : replacing ? "Update it" : "Add this site"}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/** How long each frequency of a system looked for is listened to, s. */
const LISTEN: { s: number; label: string }[] = [
  { s: 2, label: "2 s" },
  { s: 5, label: "5 s" },
  { s: 30, label: "30 s" },
  { s: 60, label: "1 min" },
  { s: 120, label: "2 min" },
  { s: 600, label: "10 min" },
];

/** A spare radio, for the radio menu: "Airspy 3A… (spare)". */
const spareLabel = (x: Source) => `${SOURCE_KINDS[x.type].label}${deviceId(x) ? ` ${deviceId(x)}` : ""} (spare)`;

export function SurveyPanel(props: { c: Config }) {
  const s = useApp();
  const { c } = props;
  const sv = s.survey;
  const recording = holdsRadios(s.phase);
  const [open, setOpen] = useState(() => c.systems.length === 0);
  const [picked, setPicked] = useState<string[] | null>(null);
  const [findGain, setFindGain] = useState(true);
  const [listenS, setListenS] = useState<number | null>(null);
  const lookingFor = s.surveyFor;
  const sought = lookingFor?.fcc ?? null;
  const bands = picked ?? (lookingFor ? bandsHolding(lookedFreqs(lookingFor), s.surveyBands) : s.surveyBands.filter((b) => b.defaultOn).map((b) => b.id));
  // The radio: a source ("0", "1", …), or a spare one no source uses ("spare:…"), which may scan while recording.
  const spares = spareRadios(s);
  const [radioPick, setRadioPick] = useState<string | null>(null);
  const radioKey = radioPick ?? (recording && spares.length ? `spare:${sourceKey(spares[0])}` : "0");
  const spare = spares.find((x) => `spare:${sourceKey(x)}` === radioKey);
  const source = spare ? -1 : Math.min(Number(radioKey) || 0, c.sources.length - 1);
  // Which spare radios power an LNA through their bias-T for the scan (a source's bias-T is its own setting).
  const [spareBias, setSpareBias] = useState<ReadonlySet<string>>(new Set());
  const spareBiasOn = !!spare && spareBias.has(sourceKey(spare));
  const scanWith = spare && hasBiasT(spare) ? { ...spare, biasTee: spareBiasOn } : spare;
  const src = spare ?? c.sources[source];
  const active = sv.stage !== "idle";
  const unsupported = !can("drivers") && !!src && needsDriver(src);
  const bandLabel = (id: string) => s.surveyBands.find((b) => b.id === id)?.label ?? id;
  const shown = !active && (open || !!lookingFor);
  // The radios plugged in now: which are spare.
  useEffect(() => {
    if (shown && can("drivers")) refreshDevices();
  }, [shown]);
  // Looking for a system and listening to another's control channel: once the radio's error is known (so the scan's
  // frequencies can be matched to its licenses), its own strongest control channel instead; once a scan.
  const switched = useRef(false);
  const monitor = sv.stage === "idle" ? null : sv.monitor;
  useEffect(() => {
    if (sv.stage === "scanning") switched.current = false;
    if (!sought || sv.stage !== "monitoring" || !monitor || switched.current || monitor.ppm === null) return;
    if (soughtOn(sought, monitor.advertisedHz ?? monitor.heardHz)) return void (switched.current = true);
    const own = sv.candidates.filter((x) => isControl(x) && soughtOn(sought, heardHz(x))).sort((a, b) => b.snrDb - a.snrDb)[0];
    if (!own) return;
    switched.current = true;
    surveyListen(own.freqHz);
  }, [sought, sv.stage, monitor?.ppm, monitor?.advertisedHz]);

  if (!s.surveyBands.length) return null;
  const freqs = lookingFor ? lookedFreqs(lookingFor).length : 0;
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
      {lookingFor && <LookingForBanner l={lookingFor} c={c} />}
      {shown && (
        <div className="stack">
          <p className="muted small">
            {lookingFor ? (
              <>
                Listens to its <Fig v={freqs} u={freqs === 1 ? "frequency" : "frequencies"} />, a band at a time
              </>
            ) : (
              "Finds P25, SmartNet, EDACS, DMR and NXDN control channels"
            )}
          </p>
          {(c.sources.length > 1 || spares.length > 0 || src?.type === "rtlsdr" || !!lookingFor) && (
          <div className="grid2">
            {(c.sources.length > 1 || spares.length > 0) && (
              <label className="field">
                <span className="field-label">Radio</span>
                <select value={radioKey} onChange={(e) => setRadioPick(e.target.value)}>
                  {c.sources.map((x, i) => (
                    <option key={i} value={i} disabled={recording}>
                      {sourceTitle(c, i)} ({SOURCE_KINDS[x.type].label}){recording ? " — recording" : ""}
                    </option>
                  ))}
                  {spares.map((x) => (
                    <option key={sourceKey(x)} value={`spare:${sourceKey(x)}`}>
                      {spareLabel(x)}
                    </option>
                  ))}
                </select>
                {spare && <span className="field-hint">Spare: scans while the others record</span>}
              </label>
            )}
            {lookingFor && (
              <label className="field">
                <span className="field-label">Listen to each frequency for</span>
                <select value={listenS ?? defaultListenS(lookingFor.type)} onChange={(e) => setListenS(Number(e.target.value))}>
                  {LISTEN.map((x) => (
                    <option key={x.s} value={x.s}>
                      {x.label}
                    </option>
                  ))}
                </select>
                <span className="field-hint">{lookingFor.type === "dmr" ? "Quiet DMR sites need longer" : "Control channels answer within seconds"}</span>
              </label>
            )}
            {spare && (
              <BiasT
                src={spare}
                checked={spareBiasOn}
                onChange={(v) =>
                  setSpareBias((x) => {
                    const n = new Set(x);
                    if (v) n.add(sourceKey(spare));
                    else n.delete(sourceKey(spare));
                    return n;
                  })
                }
              />
            )}
            {src?.type === "rtlsdr" && (
              <label className="toggle">
                <input type="checkbox" checked={findGain} onChange={(e) => setFindGain(e.target.checked)} />
                <span>
                  Find the best gain <span className="field-hint">— adds ~15 s</span>
                </span>
              </label>
            )}
          </div>
          )}
          {src?.type === "file" ? (
            <p className="muted small">Scans at its center: {src.centerHz ? <Fig v={formatMhz(src.centerHz, 4)} u="MHz" /> : "set it below"}</p>
          ) : !lookingFor && (
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
          {unsupported && <p className="warn small">{SOURCE_KINDS[src.type].label} needs the desktop app.</p>}
          <div className="row">
            <button
              className="btn primary"
              disabled={!s.connected || !src || (recording && !spare) || unsupported || (src.type !== "file" && !lookingFor && !bands.length)}
              onClick={() => startSurvey(scanWith ?? source, bands, src?.type === "rtlsdr" && findGain, listenS ?? undefined)}
            >
              Scan
            </button>
            {recording && !spare && <span className="muted small">Recording: stop it, or plug in a spare radio</span>}
          </div>
        </div>
      )}
      {sv.stage !== "idle" && (
        <div className="stack">
          {sv.radio && (
            <p className="small muted">
              Spare radio <b>{sv.radio}</b>
              {recording ? " · beside the recording" : ""}
            </p>
          )}
          {sv.error && <div className="banner bad small">{sv.error}</div>}
          {sv.stage === "scanning" && sv.progress && (
            <div className="stack">
              <div className="row small">
                <span>
                  {sv.lookFor ? (
                    <>
                      <Figs
                        inline
                        items={[
                          { k: "Frequencies", v: `${sv.lookFor.probed}/${sv.lookFor.freqs}` },
                          { k: "Each", v: sv.lookFor.listenS, u: "s" },
                          { k: "Around", v: formatMhz(sv.progress.centerHz, 3), u: "MHz" },
                        ]}
                      />
                    </>
                  ) : (
                    <>
                      Scanning <b>{bandLabel(sv.progress.band)}</b> · <Fig v={formatMhz(sv.progress.centerHz, 3)} u="MHz" />
                    </>
                  )}
                </span>
                <span className="spacer" />
                <span className="muted">
                  Step <Fig v={`${sv.progress.hop}/${sv.progress.hops}`} />
                </span>
              </div>
              <progress className="survey-progress" value={sv.progress.hop - 1} max={sv.progress.hops} />
            </div>
          )}
          {lookingFor && !!sv.sites?.length && <SiteChecks l={lookingFor} sites={sv.sites} done={sv.stage !== "scanning"} />}
          {sv.monitor && (
            <>
              <div className="row">
                <span>
                  {sv.stage === "monitoring" ? "Listening to" : "Listened to"} <span className="mono">{formatMhz(sv.monitor.advertisedHz ?? sv.monitor.heardHz)}</span> MHz
                  {sv.stage === "monitoring" && <span className="muted small"> · {sv.monitor.elapsedS.toFixed(0)} s</span>}
                </span>
              </div>
              {sv.stage === "monitoring" && s.surveySpectrum && <Waterfall radio={s.surveySpectrum} label="Survey" ccs={[{ hz: sv.monitor.heardHz, label: "CC", color: "var(--accent)" }]} calls={[]} />}
              <MonitorView m={sv.monitor} sug={sv.suggest} c={c} source={sv.source ?? -1} onDone={() => setOpen(false)} sought={sought} />
            </>
          )}
          {sv.message && <div className="banner subtle">{sv.message}</div>}
          {(sv.candidates.length > 0 || !sv.monitor) && (
            <details className="help" open={!sv.monitor && !sv.sites?.length}>
              <summary>
                Signals found <Fig v={sv.candidates.filter(isControl).length} u={sv.candidates.filter(isControl).length === 1 ? "control channel" : "control channels"} />
              </summary>
              <Candidates c={c} list={sv.candidates} listening={sv.monitor?.freqHz ?? null} onListen={surveyListen} licenses={s.surveyLicenses} sought={sought} />
            </details>
          )}
          <IdentifyBar licenses={s.surveyLicenses} canIdentify={s.connected && sv.candidates.length > 0} />
        </div>
      )}
    </section>
  );
}

const VERDICT: Record<SurveySite["verdict"], string> = {
  active: "Heard",
  otherSite: "Another site",
  unconfirmed: "Unconfirmed",
  inactive: "Not heard",
  notChecked: "Not checked",
};
/** Sites worth keeping: heard, or something there (unconfirmed). */
const heard = (x: SurveySite | undefined) => x?.verdict === "active" || x?.verdict === "unconfirmed";
const dead = (x: SurveySite | undefined) => x?.verdict === "inactive" || x?.verdict === "otherSite";

/**
 * Each site looked for: heard here or not, and why, coloured by how well it
 * decoded. Once listened to: a bundle's heard sites to import, a system's
 * sites not heard to turn off (and heard ones that are off, on).
 */
export function SiteChecks(props: { l: LookingFor; sites: SurveySite[]; done: boolean }) {
  const { l, sites } = props;
  const keep = l.sites.filter((_, k) => heard(sites[k]));
  const drop = l.sites.filter((_, k) => dead(sites[k]));
  const app = useApp();
  const sys = l.system ? app.config?.systems.find((x) => x.shortName === l.system) : undefined;
  const off = sys ? keep.filter((x) => sys.sites[x.index] && !sys.sites[x.index].enabled) : [];
  const setEnabled = (list: typeof keep, on: boolean) => {
    updateConfig((x) => {
      const s = x.systems.find((y) => y.shortName === l.system);
      for (const k of list) if (s?.sites[k.index]) s.sites[k.index].enabled = on;
    });
    setNotice(`Turned ${on ? "on" : "off"} ${list.length} of ${l.system}'s sites: ${list.map((x) => x.label).join(", ")}.`);
  };
  const plural = (n: number) => `${n} site${n === 1 ? "" : "s"}`;
  return (
    <div className="stack">
      <div className="table-wrap">
        <table className="calls site-checks">
          <thead>
            <tr>
              <th>Site</th>
              <th>Here</th>
              <th>Why</th>
              <th>MHz</th>
              <th>Signal</th>
            </tr>
          </thead>
          <tbody>
            {l.sites.map((x, k) => {
              const v = sites[k];
              return (
                <tr key={k} className={v?.quality ? `q-${v.quality}` : ""}>
                  <td>{x.label}</td>
                  <td>
                    <span className={`tag verdict-${v?.verdict ?? "notChecked"}`}>{VERDICT[v?.verdict ?? "notChecked"]}</span>
                  </td>
                  <td className="small">{v?.why ?? ""}</td>
                  <td className="mono small">{v?.freqHz ? formatMhz(v.freqHz) : ""}</td>
                  <td className="mono small">{v?.snrDb != null ? `${v.snrDb.toFixed(0)} dB` : ""}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
      {props.done && (
        <div className="row">
          <Figs
            inline
            items={[
              { k: "Heard here", v: `${keep.length}/${l.sites.length}`, u: "sites" },
              drop.length > 0 && { k: "Not heard", v: drop.length, level: "warn" },
            ]}
          />
          <span className="spacer" />
          {l.bundle && (
            <button className="btn primary small" disabled={!keep.length} onClick={() => openImport(l.bundle!, keep.map((x) => x.index))}>
              Import the {plural(keep.length)} heard…
            </button>
          )}
          {sys && off.length > 0 && (
            <button className="btn small" onClick={() => setEnabled(off, true)}>
              Turn on the {plural(off.length)} heard
            </button>
          )}
          {sys && drop.filter((x) => sys.sites[x.index]?.enabled).length > 0 && (
            <button className="btn small" onClick={() => setEnabled(drop.filter((x) => sys.sites[x.index]?.enabled), false)} title="Turned off, not removed: turn them back on in its sites">
              Turn off the {plural(drop.filter((x) => sys.sites[x.index]?.enabled).length)} not heard
            </button>
          )}
        </div>
      )}
    </div>
  );
}

/**
 * What the survey looks for: a system known only by its licenses (once a
 * system is it, findExisting: its call sign, sharing it), or which of a
 * system's sites can be heard here.
 */
function LookingForBanner(props: { l: LookingFor; c: Config }) {
  const { l, c } = props;
  const b = l.bundle;
  if (!b || !toFind(b)) {
    const n = l.sites.length;
    return (
      <div className="banner">
        <span>
          Checking <b>{l.name}</b>: which of its <Fig v={n} u={n === 1 ? "site" : "sites"} /> are heard here{" "}
          <Info>
            Each one's {l.type === "dmr" ? "frequencies" : "control channels"} listened to. {l.system ? "Its sites not heard can be turned off." : "The ones heard can be imported."}
          </Info>
        </span>
        <span className="row">
          <button className="btn ghost small" onClick={() => lookFor(null)}>
            Stop looking
          </button>
        </span>
      </div>
    );
  }
  const n = Object.keys(l.fcc ?? {}).length;
  const call = fccPrimary(l.fcc ?? undefined, [])?.callSign;
  const found = findExisting(c, b);
  const sys = found >= 0 ? c.systems[found] : null;
  return (
    <div className="banner">
      {sys ? (
        <span>
          <b>{sys.shortName}</b> is {l.name} · <span className="mono">{call}</span>{" "}
          {sys.share ? (
            <span className="chip ok" title="Shared with the directory once it has decoded a while">
              Sharing
            </span>
          ) : (
            <>
              <span className="chip">Not shared yet</span> <Info>Share it, and it's on the directory's map for others.</Info>
            </>
          )}
        </span>
      ) : (
        <span>
          Looking for <b>{l.name}</b> <span className="mono">{call}</span> · <Fig v={n} u={n === 1 ? "licensed frequency" : "licensed frequencies"} />{" "}
          <Info>Signals found on them are marked as its own.</Info>{" "}
          {b.source.url && (
            <a href={b.source.url} target="_blank" rel="noreferrer">
              In the directory
            </a>
          )}
        </span>
      )}
      <span className="row">
        {sys && !sys.share && (
          <button className="btn small" onClick={() => updateConfig((x) => void (x.systems[found].share = true))}>
            Share it
          </button>
        )}
        <button className="btn ghost small" onClick={() => lookFor(null)}>
          {sys ? "Done" : "Stop looking"}
        </button>
      </span>
    </div>
  );
}
