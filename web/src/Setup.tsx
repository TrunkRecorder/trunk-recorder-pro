import { useEffect, useRef, useState } from "react";
import { autoCenter, formatMhz, importTrunkRecorderConfig, newDongle, parseFreqList, resolvedCenters, SAMPLE_RATES, usableHalfWidth } from "./config.ts";
import { refreshDevices, setNotice, updateConfig, useApp, web } from "./controller.ts";
import type { Config, Source } from "./protocol.ts";
import { parseTalkgroupCsv } from "./talkgroups.ts";

function Field(props: { label: string; hint?: string; children: React.ReactNode; wide?: boolean }) {
  return (
    <label className={`field${props.wide ? " wide" : ""}`}>
      <span className="field-label">{props.label}</span>
      {props.children}
      {props.hint && <span className="field-hint">{props.hint}</span>}
    </label>
  );
}

function Toggle(props: { label: string; checked: boolean; onChange: (v: boolean) => void; hint?: string }) {
  return (
    <label className="toggle">
      <input type="checkbox" checked={props.checked} onChange={(e) => props.onChange(e.target.checked)} />
      <span>
        {props.label}
        {props.hint && <span className="field-hint"> — {props.hint}</span>}
      </span>
    </label>
  );
}

/** A frequency input that keeps the user's text while typing. */
function MhzInput(props: { hz: number; placeholder?: string; onChange: (hz: number) => void }) {
  const [text, setText] = useState(props.hz ? formatMhz(props.hz) : "");
  return (
    <input
      className="mono"
      value={text}
      placeholder={props.placeholder ?? "MHz"}
      onChange={(e) => {
        setText(e.target.value);
        props.onChange(parseFreqList(e.target.value)[0] ?? 0);
      }}
    />
  );
}

async function connectDongle(): Promise<void> {
  try {
    await web?.requestUsb();
  } catch (e) {
    setNotice(e instanceof Error ? e.message : String(e));
  }
}

function SourceCard(props: { c: Config; i: number }) {
  const s = useApp();
  const { c, i } = props;
  const src = c.sources[i];
  const center = resolvedCenters(c)[i];
  const auto = i === 0 ? autoCenter(c.system.controlChannels, src.rateHz) : null;
  const half = usableHalfWidth(src.rateHz);
  const edit = (fn: (x: Source) => void) => updateConfig((x) => fn(x.sources[i]));
  const setKind = (kind: Source["kind"]) =>
    updateConfig((x) => {
      const old = x.sources[i];
      x.sources[i] =
        kind === "rtlsdr" ? { ...newDongle(), centerHz: old.centerHz, rateHz: old.rateHz } : { kind: "file", path: "", centerHz: old.centerHz, rateHz: old.rateHz, realtime: true };
    });
  const fileRef = useRef<HTMLInputElement>(null);
  const used = new Set(c.sources.filter((x, k) => k !== i && x.kind === "rtlsdr").map((x) => (x.kind === "rtlsdr" ? x.serial : "")));

  return (
    <div className="source-card">
      <div className="row source-head">
        <strong>Source {i + 1}</strong>
        <div className="seg small" role="radiogroup" aria-label={`Source ${i + 1} kind`}>
          <button role="radio" aria-checked={src.kind === "rtlsdr"} className={src.kind === "rtlsdr" ? "on" : ""} onClick={() => setKind("rtlsdr")}>
            RTL-SDR dongle
          </button>
          <button role="radio" aria-checked={src.kind === "file"} className={src.kind === "file" ? "on" : ""} onClick={() => setKind("file")}>
            Capture file
          </button>
        </div>
        <span className="spacer" />
        {c.sources.length > 1 && (
          <button
            className="btn ghost small"
            onClick={() => {
              web?.removeFile(i);
              updateConfig((x) => void x.sources.splice(i, 1));
            }}
          >
            Remove
          </button>
        )}
      </div>
      <div className="grid2">
        {src.kind === "rtlsdr" ? (
          <Field
            label="Dongle"
            hint={s.devices.length ? undefined : web ? "No dongle connected — press Connect and pick it in the browser's list." : "No dongle found — plug one in and press Refresh."}
          >
            <div className="row">
              <select value={src.serial} onChange={(e) => edit((x) => x.kind === "rtlsdr" && void (x.serial = e.target.value))}>
                <option value="">First available</option>
                {s.devices.map((d) => (
                  <option key={d.serial || d.index} value={d.serial} disabled={used.has(d.serial)}>
                    {d.product}
                    {d.serial ? ` · SN ${d.serial}` : ""}
                    {used.has(d.serial) ? " (in use)" : ""}
                  </option>
                ))}
              </select>
              {web ? (
                <button className="btn ghost small" onClick={() => void connectDongle()}>
                  Connect…
                </button>
              ) : (
                <button className="btn ghost small" onClick={refreshDevices}>
                  Refresh
                </button>
              )}
            </div>
          </Field>
        ) : web ? (
          <Field label="Capture file" hint="rtl_sdr output (unsigned 8-bit IQ). The browser forgets the choice on reload." wide>
            <div className="row">
              <button className="btn ghost small" onClick={() => fileRef.current?.click()}>
                Choose file…
              </button>
              <span className="mono">{web.file(i)?.name ?? (src.path ? `${src.path} (choose again)` : "none")}</span>
            </div>
            <input
              ref={fileRef}
              type="file"
              hidden
              onChange={(e) => {
                const f = e.target.files?.[0] ?? null;
                web?.setFile(i, f);
                edit((x) => x.kind === "file" && void (x.path = f?.name ?? ""));
                e.target.value = "";
              }}
            />
          </Field>
        ) : (
          <Field label="Capture file" hint="rtl_sdr output (unsigned 8-bit IQ), a path on the recorder's computer." wide>
            <input className="mono" value={src.path} placeholder="/path/to/capture.cu8" onChange={(e) => edit((x) => x.kind === "file" && void (x.path = e.target.value))} />
          </Field>
        )}
        <Field label="Center frequency, MHz" hint={src.centerHz ? "Manual" : auto ? `Auto: ${formatMhz(auto, 4)} MHz` : i === 0 ? "Auto — needs control channels" : "Required"}>
          <MhzInput hz={src.centerHz} placeholder={auto ? formatMhz(auto, 4) : "MHz"} onChange={(hz) => edit((x) => void (x.centerHz = hz))} />
        </Field>
        <Field label="Sample rate">
          <select value={src.rateHz} onChange={(e) => edit((x) => void (x.rateHz = Number(e.target.value)))}>
            {SAMPLE_RATES.map((r) => (
              <option key={r} value={r}>
                {(r / 1e6).toFixed(3)} MSPS
              </option>
            ))}
          </select>
        </Field>
        {src.kind === "rtlsdr" ? (
          <>
            <Field label="Gain, dB" hint="Blank = tuner AGC">
              <input
                className="mono"
                value={src.gainDb ?? ""}
                placeholder="AGC"
                onChange={(e) => {
                  const v = e.target.value.trim();
                  edit((x) => x.kind === "rtlsdr" && void (x.gainDb = v === "" ? null : Number(v)));
                }}
              />
            </Field>
            <Field label="Frequency correction, ppm">
              <input className="mono" value={src.ppm} onChange={(e) => edit((x) => x.kind === "rtlsdr" && void (x.ppm = Number(e.target.value) || 0))} />
            </Field>
          </>
        ) : (
          <Toggle label="Real-time pace" hint="off = as fast as the computer decodes" checked={src.realtime} onChange={(v) => edit((x) => x.kind === "file" && void (x.realtime = v))} />
        )}
      </div>
      {center ? (
        <p className="muted small">
          Covers {formatMhz(center - half, 3)} – {formatMhz(center + half, 3)} MHz.
        </p>
      ) : null}
    </div>
  );
}

export function Setup() {
  const s = useApp();
  const c = s.config;
  const [ccText, setCcText] = useState(() => (c ? c.system.controlChannels.map((f) => formatMhz(f)).join(", ") : ""));
  // The config arrives from the recorder after the first render.
  const loaded = c !== null;
  useEffect(() => {
    if (loaded && !ccText) setCcText((s.config?.system.controlChannels ?? []).map((f) => formatMhz(f)).join(", "));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loaded]);
  const importRef = useRef<HTMLInputElement>(null);
  const tgRef = useRef<HTMLInputElement>(null);
  if (!c) return <p className="muted">Connecting to the recorder…</p>;
  const tgCount = c.system.talkgroupsCsv ? parseTalkgroupCsv(c.system.talkgroupsCsv).size : 0;

  const onImport = async (f: File | undefined) => {
    if (!f) return;
    try {
      const { config, notes } = importTrunkRecorderConfig(await f.text(), c);
      updateConfig((x) => Object.assign(x, config));
      setCcText(config.system.controlChannels.map((v) => formatMhz(v)).join(", "));
      setNotice(`Imported ${f.name}.${notes.length ? " " + notes.join(" ") : ""}`);
    } catch (e) {
      setNotice(`Couldn't read ${f.name}: ${e instanceof Error ? e.message : String(e)}`);
    }
  };

  const onTalkgroups = async (f: File | undefined) => {
    if (!f) return;
    const text = await f.text();
    const n = parseTalkgroupCsv(text).size;
    updateConfig((x) => {
      x.system.talkgroupsCsv = text;
      x.system.talkgroupsName = f.name;
    });
    setNotice(`Loaded ${n} talkgroups from ${f.name}.`);
  };

  return (
    <div className="setup">
      <section className="panel">
        <header className="panel-head">
          <h2>System</h2>
          <button className="btn ghost" onClick={() => importRef.current?.click()}>
            Import Trunk Recorder config…
          </button>
          <input ref={importRef} type="file" accept=".json,application/json" hidden onChange={(e) => void onImport(e.target.files?.[0])} />
        </header>
        <div className="grid2">
          <Field label="Short name" hint="Folder name for this system's calls">
            <input value={c.system.shortName} onChange={(e) => updateConfig((x) => void (x.system.shortName = e.target.value.replace(/[^\w.-]/g, "") || "sys1"))} />
          </Field>
          <Field label="Type">
            <select value="p25" disabled>
              <option value="p25">P25 (Phase 1)</option>
            </select>
          </Field>
          <Field label="Control channels, MHz" hint="Comma separated. The first one in range is tried first; the rest are fallbacks." wide>
            <input
              className="mono"
              value={ccText}
              placeholder="857.9875, 858.9875"
              onChange={(e) => {
                setCcText(e.target.value);
                const list = parseFreqList(e.target.value);
                updateConfig((x) => void (x.system.controlChannels = list));
              }}
            />
          </Field>
          <Field label="Modulation" hint="Auto runs C4FM and CQPSK receivers side by side and keeps the best of each frame.">
            <select value={c.system.modulation} onChange={(e) => updateConfig((x) => void (x.system.modulation = e.target.value as Config["system"]["modulation"]))}>
              <option value="auto">Auto (both receivers)</option>
              <option value="fsk4">C4FM (fsk4)</option>
              <option value="qpsk">CQPSK / LSM simulcast (qpsk)</option>
            </select>
          </Field>
          <Field label="Talkgroups" hint="Trunk Recorder's talkgroup CSV">
            <div className="row">
              <button className="btn" onClick={() => tgRef.current?.click()}>
                Load CSV…
              </button>
              <span className="mono">{tgCount ? `${tgCount} from ${c.system.talkgroupsName}` : "none"}</span>
              {tgCount > 0 && (
                <button
                  className="btn ghost"
                  onClick={() =>
                    updateConfig((x) => {
                      x.system.talkgroupsCsv = "";
                      x.system.talkgroupsName = "";
                    })
                  }
                >
                  Clear
                </button>
              )}
            </div>
            <input ref={tgRef} type="file" accept=".csv,text/csv" hidden onChange={(e) => void onTalkgroups(e.target.files?.[0])} />
          </Field>
        </div>
      </section>

      <section className="panel">
        <header className="panel-head">
          <h2>Sources</h2>
          <button className="btn ghost" onClick={() => updateConfig((x) => void x.sources.push(newDongle()))}>
            Add a dongle
          </button>
        </header>
        <div className="stack">
          {c.sources.map((_, i) => (
            <SourceCard key={i} c={c} i={i} />
          ))}
          <p className="muted small">
            Several dongles can cover one system: the control channel runs on whichever covers it, and each call is recorded from whichever covers its frequency.
          </p>
          <details className="help">
            <summary>Dongle not showing up?</summary>
            {web ? (
              <ul>
                <li>The browser version needs Chrome or Edge (WebUSB). Press Connect and choose the dongle; the browser remembers it for this site.</li>
                <li>
                  <b>Windows:</b> install the WinUSB driver for “Bulk-In, Interface 0” with Zadig.
                </li>
                <li>
                  <b>Linux:</b> add a udev rule giving your user access to USB 0bda:2838 and unload <code>dvb_usb_rtl28xxu</code>.
                </li>
                <li>For several dongles or long unattended runs, the desktop app is sturdier.</li>
              </ul>
            ) : (
            <ul>
              <li>
                <b>Windows:</b> install the WinUSB driver for “Bulk-In, Interface 0” with Zadig (the same step every RTL-SDR app needs).
              </li>
              <li>
                <b>Linux:</b> add a udev rule giving your user access to USB 0bda:2838, and unload the DVB driver (<code>sudo rmmod dvb_usb_rtl28xxu</code>) if it holds the dongle.
              </li>
              <li>
                <b>macOS:</b> works as-is. Quit other SDR apps that hold the dongle.
              </li>
            </ul>
            )}
          </details>
        </div>
      </section>

      <section className="panel">
        <header className="panel-head">
          <h2>Recording</h2>
        </header>
        <div className="stack">
          {web ? (
            <p className="muted small">Calls are kept in this browser's storage (<i>Recorded calls</i> → Export to folder copies them out in Trunk Recorder's layout).</p>
          ) : (
            <Field label="Recordings folder" hint="On the recorder's computer. Calls go to <folder>/<short name>/<year>/<month>/<day>/." wide>
              <input className="mono" value={c.recording.captureDir} onChange={(e) => updateConfig((x) => void (x.recording.captureDir = e.target.value))} />
            </Field>
          )}
          <Toggle label="Record talkgroups not in the CSV" checked={c.recording.recordUnknown} onChange={(v) => updateConfig((x) => void (x.recording.recordUnknown = v))} />
          <Toggle label="Record unit-to-unit calls" checked={c.recording.recordUnitToUnit} onChange={(v) => updateConfig((x) => void (x.recording.recordUnitToUnit = v))} />
          <Toggle label="Keep calls with no audio" hint="encrypted, or nothing decoded" checked={c.recording.keepSilentCalls} onChange={(v) => updateConfig((x) => void (x.recording.keepSilentCalls = v))} />
          <div className="grid3">
            <Field label="Pre-roll, s" hint="Air replayed from before the grant">
              <input className="mono" value={c.recording.prerollS} onChange={(e) => updateConfig((x) => void (x.recording.prerollS = Math.max(0, Math.min(3, Number(e.target.value) || 0))))} />
            </Field>
            <Field label="Call timeout, s">
              <input className="mono" value={c.recording.callTimeoutS} onChange={(e) => updateConfig((x) => void (x.recording.callTimeoutS = Math.max(1, Number(e.target.value) || 3)))} />
            </Field>
            <Field label="Max recorders">
              <input className="mono" value={c.recording.maxRecorders} onChange={(e) => updateConfig((x) => void (x.recording.maxRecorders = Math.max(1, Math.min(64, Number(e.target.value) || 32))))} />
            </Field>
          </div>
        </div>
      </section>
    </div>
  );
}
