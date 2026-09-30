import { useRef, useState } from "react";
import { autoCenter, formatMhz, importTrunkRecorderConfig, parseFreqList, SAMPLE_RATES, usableHalfWidth } from "../config.ts";
import { parseTalkgroupCsv } from "../trunking/talkgroups.ts";
import { grantDevice, setNotice, setSource, updateConfig, useApp } from "./controller.ts";

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

export function Setup() {
  const s = useApp();
  const c = s.config;
  const [ccText, setCcText] = useState(() => c.system.controlChannels.map((f) => formatMhz(f)).join(", "));
  const importRef = useRef<HTMLInputElement>(null);
  const tgRef = useRef<HTMLInputElement>(null);
  const fileRef = useRef<HTMLInputElement>(null);
  const hasUsb = "usb" in navigator;

  const auto = autoCenter(c.system.controlChannels, c.source.rateHz);
  const center = c.source.centerHz || auto;
  const half = usableHalfWidth(c.source.rateHz);
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
          <h2>Source</h2>
        </header>
        <div className="seg" role="radiogroup" aria-label="Source">
          <button role="radio" aria-checked={s.sourceKind === "usb"} className={s.sourceKind === "usb" ? "on" : ""} onClick={() => setSource("usb")}>
            RTL-SDR dongle
          </button>
          <button role="radio" aria-checked={s.sourceKind === "file"} className={s.sourceKind === "file" ? "on" : ""} onClick={() => setSource("file")}>
            Replay a capture
          </button>
        </div>
        {s.sourceKind === "usb" ? (
          <div className="stack">
            {!hasUsb && <p className="warn">This browser has no WebUSB. Use Chrome or Edge (desktop, or Android with a USB-OTG adapter).</p>}
            {s.devices.length ? (
              <Field label="Dongle">
                <select value={c.source.serial} onChange={(e) => updateConfig((x) => void (x.source.serial = e.target.value))}>
                  <option value="">First available</option>
                  {s.devices.map((d) => (
                    <option key={d.serial || d.product} value={d.serial}>
                      {d.product}
                      {d.serial ? ` · SN ${d.serial}` : ""}
                    </option>
                  ))}
                </select>
              </Field>
            ) : (
              <p className="muted">No dongle connected to this page yet.</p>
            )}
            <div>
              <button className="btn" disabled={!hasUsb} onClick={() => void grantDevice()}>
                {s.devices.length ? "Connect another dongle…" : "Connect dongle…"}
              </button>
            </div>
            <details className="help">
              <summary>Dongle not showing up?</summary>
              <ul>
                <li>
                  <b>Windows:</b> install the WinUSB driver for “Bulk-In, Interface 0” with Zadig (the same step every RTL-SDR app needs).
                </li>
                <li>
                  <b>Linux:</b> the DVB kernel driver grabs the dongle. Run <code>sudo rmmod dvb_usb_rtl28xxu</code> (or blacklist it), and add a udev rule giving your user access to USB 0bda:2838.
                </li>
                <li>
                  <b>macOS:</b> works as-is. Quit other SDR apps that hold the dongle.
                </li>
              </ul>
            </details>
          </div>
        ) : (
          <div className="stack">
            <Field label="Capture file" hint="rtl_sdr output: unsigned 8-bit interleaved IQ (.cu8 / .bin). Set its center and sample rate under Tuning.">
              <div className="row">
                <button className="btn" onClick={() => fileRef.current?.click()}>
                  Choose file…
                </button>
                <span className="mono">{s.file ? `${s.file.name} · ${(s.file.size / 1e6).toFixed(0)} MB` : "none"}</span>
              </div>
              <input ref={fileRef} type="file" hidden onChange={(e) => setSource("file", e.target.files?.[0] ?? null)} />
            </Field>
            <Toggle label="Real-time pace" hint="off = as fast as this computer can decode" checked={s.realtime} onChange={(v) => setSource("file", undefined, v)} />
          </div>
        )}
      </section>

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
              <option value="p25">P25 (Phase 1 & 2)</option>
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
          <Field label="Modulation" hint="Simulcast sites use CQPSK. Auto measures it.">
            <select value={c.system.modulation} onChange={(e) => updateConfig((x) => void (x.system.modulation = e.target.value as typeof c.system.modulation))}>
              <option value="auto">Auto-detect</option>
              <option value="fsk4">C4FM (fsk4)</option>
              <option value="qpsk">CQPSK / LSM (qpsk)</option>
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
          <h2>Tuning</h2>
        </header>
        <div className="grid2">
          <Field label="Center frequency, MHz" hint={c.source.centerHz ? "Manual" : auto ? `Auto: ${formatMhz(auto, 4)} MHz` : "Auto — needs control channels"}>
            <input
              className="mono"
              value={c.source.centerHz ? formatMhz(c.source.centerHz) : ""}
              placeholder={auto ? formatMhz(auto, 4) : "auto"}
              onChange={(e) => {
                const v = parseFreqList(e.target.value)[0] ?? 0;
                updateConfig((x) => void (x.source.centerHz = v));
              }}
            />
          </Field>
          <Field label="Sample rate">
            <select value={c.source.rateHz} onChange={(e) => updateConfig((x) => void (x.source.rateHz = Number(e.target.value)))}>
              {SAMPLE_RATES.map((r) => (
                <option key={r} value={r}>
                  {(r / 1e6).toFixed(3)} MSPS
                </option>
              ))}
            </select>
          </Field>
          <Field label="Gain, dB" hint="Blank = tuner AGC">
            <input
              className="mono"
              value={c.source.gainDb ?? ""}
              placeholder="AGC"
              onChange={(e) => {
                const v = e.target.value.trim();
                updateConfig((x) => void (x.source.gainDb = v === "" ? null : Number(v)));
              }}
            />
          </Field>
          <Field label="Frequency correction, ppm">
            <input className="mono" value={c.source.ppm} onChange={(e) => updateConfig((x) => void (x.source.ppm = Number(e.target.value) || 0))} />
          </Field>
        </div>
        {center ? (
          <p className="muted small">
            Covers {formatMhz(center - half, 3)} – {formatMhz(center + half, 3)} MHz. Calls granted outside this range are logged but can't be recorded.
          </p>
        ) : null}
      </section>

      <section className="panel">
        <header className="panel-head">
          <h2>Recording</h2>
        </header>
        <div className="stack">
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
