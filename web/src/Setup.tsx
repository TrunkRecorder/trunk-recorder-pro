import { Fragment, useEffect, useId, useRef, useState } from "react";
import {
  activeSystems,
  AIRSPY_RATES,
  channelsToCsv,
  channelTalkgroups,
  defaultLog,
  enabledChannels,
  formatFromPath,
  formatGain,
  formatMhz,
  mhzCell,
  newAirspy,
  newConventional,
  newDongle,
  newFile,
  newSoapy,
  newUsrp,
  nextTalkgroup,
  parseChannelCsv,
  parseFreqList,
  newSystem,
  resolvedCenters,
  SAMPLE_RATES,
  siteSiblings,
  SOAPY_RATES,
  sourceCovering,
  systemColor,
  USRP_RATES,
  usableHalfWidth,
  siteLockFields,
} from "./config.ts";
import { currentView, dismissTodo, downloadText, findRadios, openGuide, refreshDevices, setChannelFile, setNotice, setSetupTab, setView, updateConfig, useApp, web, type SetupTab } from "./controller.ts";
import { InterfacesPanel } from "./Interfaces.tsx";
import { M4aSettings, PluginSetupPanel, renameSystemRefs, SystemPluginSettings } from "./Plugins.tsx";
import { IconAntenna, IconDongle, IconFolder, IconPuzzle, IconTower } from "./Onboarding.tsx";
import type { AirspyGainMode, Channel, Config, Conventional, HeardCode, LogSettings, Recording, RecordingOverride, RecordingRules, SiteIdentity, SoapyState, Source, System, UnitNames } from "./protocol.ts";
import { DEFAULT_FORMAT, FilenameEditor } from "./FilenameEditor.tsx";
import { GuardBand } from "./Rolloff.tsx";
import { SurveyPanel } from "./Survey.tsx";
import { TalkgroupsField } from "./TalkgroupsField.tsx";
import { unitNameCount } from "./units.ts";
import { openTodos } from "./todo.ts";
import { parseAccess, sameTone } from "./tones.ts";

/** `needs`: what an import left to fill in here (highlighted); `anchor`: its place, for the to-do list's Show. */
function Field(props: { label: string; hint?: string; children: React.ReactNode; wide?: boolean; needs?: string; anchor?: string }) {
  return (
    <label className={`field${props.wide ? " wide" : ""}${props.needs ? " needs" : ""}`} id={props.anchor && `need-${props.anchor}`}>
      <span className="field-label">{props.label}</span>
      {props.children}
      {props.needs && <span className="field-needs">{props.needs}</span>}
      {props.hint && <span className="field-hint">{props.hint}</span>}
    </label>
  );
}

/** What an import left to do at a setup field (see todo.ts), or undefined. */
function useNeed(): (target: string) => string | undefined {
  const todos = openTodos(useApp());
  return (target) => todos.find((t) => t.target === target)?.text;
}

/** The setup tab a to-do's field is on. */
function tabOf(target: string): SetupTab {
  if (target.startsWith("plugin")) return "plugins";
  if (target.startsWith("src-")) return "radios";
  return /^(channels|squelch|convplug|convunits|conv)-/.test(target) ? "conventional" : "systems";
}

/** Show a setup field (`need-<target>`): switch to its page and tab, scroll to it and flash it. */
export function showTodo(target: string, tries = 0): void {
  if (currentView() !== "setup") {
    setView("setup");
    return void setTimeout(() => showTodo(target, tries), 60);
  }
  if (currentTab() !== tabOf(target)) {
    setSetupTab(tabOf(target));
    return void setTimeout(() => showTodo(target, tries), 60);
  }
  const el = document.getElementById(`need-${target}`);
  // Not drawn yet (a plugin just installed, say): a moment more.
  if (!el && tries < 30) return void setTimeout(() => showTodo(target, tries + 1), 100);
  if (el instanceof HTMLDetailsElement) el.open = true;
  el?.scrollIntoView({ behavior: "smooth", block: "center" });
  el?.classList.remove("flash");
  void el?.offsetWidth;
  el?.classList.add("flash");
}

/** What a Trunk Recorder import left to finish; each closes once its field is filled in. */
function TodoPanel() {
  const todos = openTodos(useApp());
  if (!todos.length) return null;
  return (
    <section className="panel todo-panel">
      <header className="panel-head">
        <h2>Finish bringing over Trunk Recorder</h2>
        <span className="muted small">{todos.length} left</span>
      </header>
      <ul className="todo-list">
        {todos.map((t, k) => (
          <li key={k}>
            <span className="todo-mark" aria-hidden="true" />
            <div className="todo-text">
              <b>{t.title}</b>
              <span className="muted small">{t.text}</span>
            </div>
            <div className="row">
              <button className="btn small" onClick={() => showTodo(t.target)}>
                {t.target.startsWith("plugin") ? "Open Plugins" : "Show"}
              </button>
              <button className="btn ghost small" onClick={() => dismissTodo(t.item)}>
                Dismiss
              </button>
            </div>
          </li>
        ))}
      </ul>
    </section>
  );
}

function Toggle(props: { label: string; checked: boolean; onChange: (v: boolean) => void; hint?: string; disabled?: boolean }) {
  return (
    <label className={`toggle${props.disabled ? " disabled" : ""}`}>
      <input type="checkbox" checked={props.checked} disabled={props.disabled} onChange={(e) => props.onChange(e.target.checked)} />
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

/** A gain in dB, shown to a tenth (the value kept may be longer); the user's text while typing. Blank = null. */
function GainInput(props: { value: number | null; placeholder?: string; disabled?: boolean; onChange: (db: number | null) => void; label?: string }) {
  const [text, setText] = useState<string | null>(null);
  const shown = props.value === null ? "" : formatGain(props.value);
  return (
    <input
      className="mono"
      value={text ?? shown}
      disabled={props.disabled}
      aria-label={props.label}
      placeholder={props.placeholder}
      onFocus={() => setText(shown)}
      onBlur={() => setText(null)}
      onChange={(e) => {
        setText(e.target.value);
        const v = e.target.value.trim();
        props.onChange(v === "" ? null : Number(v));
      }}
    />
  );
}

const TONE_HINT = {
  fm: { placeholder: "any", title: "CTCSS tone or DCS code (151.4 PL, 023 DPL). Empty: any." },
  p25: { placeholder: "any NAC", title: "NAC, hex (293). Empty: any." },
  dmr: { placeholder: "any CC", title: "Colour code, optional slot and talkgroup (CC1 TS2 TG201). Empty: any." },
  nxdn48: { placeholder: "any RAN", title: "RAN, optional talkgroup (RAN 5 TG 201). Empty or RAN 0: any." },
  nxdn96: { placeholder: "any RAN", title: "RAN, optional talkgroup (RAN 5 TG 201). Empty or RAN 0: any." },
};

/** A channel's code (CTCSS / DCS, NAC, colour code, RAN): kept as typed, tidied when left. */
function ToneInput(props: { mode: Channel["mode"]; tone: string; disabled?: boolean; onChange: (tone: string) => void }) {
  const [text, setText] = useState(props.tone);
  const parsed = parseAccess(props.mode, text);
  const error = "error" in parsed ? parsed.error : null;
  return (
    <input
      className={`mono narrow${error ? " invalid" : ""}`}
      disabled={props.disabled}
      value={text}
      placeholder={TONE_HINT[props.mode].placeholder}
      aria-label="Tone"
      aria-invalid={!!error}
      title={error ?? TONE_HINT[props.mode].title}
      onChange={(e) => {
        setText(e.target.value);
        props.onChange(e.target.value.trim());
      }}
      onBlur={() => {
        if ("tone" in parsed) {
          setText(parsed.tone);
          props.onChange(parsed.tone);
        }
      }}
    />
  );
}

const NONE_LABEL = { fm: "no tone", p25: "no NAC", dmr: "no code", nxdn48: "no code", nxdn96: "no code" };
const SHOWN_CODES = 8;

/**
 * Under a frequency's rows: the codes it has carried (tones, NACs, colour
 * codes), most heard first, each with an Add that makes a row for it — the
 * way to find a frequency's codes without knowing them.
 */
function HeardRow(props: { rows: Channel[]; heard: HeardCode[]; linked: boolean; onAdd: (code: string) => void }) {
  const mode = props.rows[0].mode;
  // Only what this mode's Tone can say (NXDN's RAN 0 is any: "RAN 0 TG 201" is "TG 201").
  const nxdn = mode === "nxdn48" || mode === "nxdn96";
  const codes = props.heard.flatMap((h) => {
    const p = parseAccess(mode, h.code);
    return "tone" in p && (p.tone === h.code || (nxdn && /^RAN 0\b/.test(h.code))) ? [{ ...h, code: p.tone }] : [];
  });
  if (!codes.length) return null;
  const listed = (code: string) => props.rows.some((r) => (code ? !!r.tone && sameTone(r.tone, code) : !r.tone));
  const label = (code: string) => (!code ? NONE_LABEL[mode] : mode === "fm" && /^\d/.test(code) ? `${code} Hz` : code);
  const shown = codes.slice(0, SHOWN_CODES);
  return (
    <tr className="heard-row">
      <td />
      <td colSpan={7}>
        <span className="muted small">Heard:</span>
        {shown.map((h) => (
          <span key={h.code} className="heard-code" title={`Last heard ${new Date(h.lastMs).toLocaleString()}`}>
            <span className="mono">{label(h.code)}</span>
            <span className="muted small">
              {" "}
              {h.calls ? `${h.calls} call${h.calls === 1 ? "" : "s"}` : ""}
              {h.calls && h.skipped ? ", " : ""}
              {h.skipped ? `${h.skipped} not recorded` : ""}
            </span>
            {listed(h.code) ? (
              <span className="muted small" title="Already listed">
                {" "}
                ✓
              </span>
            ) : (
              !props.linked && (
                <button className="btn ghost small" title={`Add a row for ${label(h.code)}`} onClick={() => props.onAdd(h.code)}>
                  Add
                </button>
              )
            )}
          </span>
        ))}
        {codes.length > SHOWN_CODES && <span className="muted small">and {codes.length - SHOWN_CODES} more</span>}
      </td>
    </tr>
  );
}

async function connectDongle(): Promise<void> {
  try {
    await web?.requestUsb();
  } catch (e) {
    setNotice(e instanceof Error ? e.message : String(e));
  }
}

/** A sample rate in MSPS: pick a common one or type another. */
function RateInput(props: { hz: number; options: number[]; onChange: (hz: number) => void; free?: boolean }) {
  const id = useId();
  if (!props.free) {
    return (
      <select value={props.hz} onChange={(e) => props.onChange(Number(e.target.value))}>
        {(props.options.includes(props.hz) ? props.options : [props.hz, ...props.options]).map((r) => (
          <option key={r} value={r}>
            {(r / 1e6).toFixed(3)} MSPS
          </option>
        ))}
      </select>
    );
  }
  return (
    <>
      <input
        className="mono"
        list={id}
        defaultValue={props.hz / 1e6}
        onChange={(e) => {
          const v = Number(e.target.value);
          if (v > 0) props.onChange(Math.round(v * 1e6));
        }}
      />
      <datalist id={id}>
        {props.options.map((r) => (
          <option key={r} value={r / 1e6} />
        ))}
      </datalist>
    </>
  );
}

/** Why an optional driver can't be used, and how to install it. */
function DriverMissing(props: { kind: "usrp" | "airspy" | "soapy"; detail: string }) {
  return (
    <div className="banner bad small wide">
      {props.kind === "soapy" ? (
        <span>
          SoapySDR support needs <b>SoapySDR</b> and your radio&apos;s module, which weren&apos;t found ({props.detail}). Install them — macOS:{" "}
          <code>brew install soapysdr soapyhackrf</code>; Debian/Ubuntu: <code>sudo apt install soapysdr0.8-module-all</code>; Windows: PothosSDR or
          radioconda — then restart Trunk Recorder Pro.
        </span>
      ) : props.kind === "usrp" ? (
        <span>
          USRP support needs <b>UHD</b>, which wasn&apos;t found ({props.detail}). Install it — macOS: <code>brew install uhd</code>; Debian/Ubuntu:{" "}
          <code>sudo apt install libuhd-dev uhd-host</code>; Windows: Ettus&apos;s UHD installer — then run <code>uhd_images_downloader</code> and restart
          Trunk Recorder Pro.
        </span>
      ) : (
        <span>
          Airspy support needs <b>libairspy</b>, which wasn&apos;t found ({props.detail}). Install it — macOS: <code>brew install airspy</code>; Debian/Ubuntu:{" "}
          <code>sudo apt install libairspy0</code>; Windows: put <code>airspy.dll</code> (from airspy-tools) next to <code>trunk-pro.exe</code> — then
          restart Trunk Recorder Pro.
        </span>
      )}
    </div>
  );
}

/**
 * SoapySDR's installed modules (one per device family): what each can open, or
 * why it didn't load. A module installed since start shows up on Find.
 */
function SoapyModules(props: { soapy: SoapyState }) {
  const { modules, searchPaths } = props.soapy;
  if (modules === undefined) return null;
  const failed = (modules ?? []).filter((m) => m.error);
  return (
    <div className="field wide">
      <span className="field-label">SoapySDR modules</span>
      {modules === null ? (
        <span className="muted small">SoapySDR 0.7 can&apos;t list modules; see SoapySDRUtil --info.</span>
      ) : modules.length === 0 ? (
        <span className="warn small">
          None installed{searchPaths?.length ? ` (looked in ${searchPaths.join(", ")})` : ""} — SoapySDR can&apos;t open any radio without one.
        </span>
      ) : (
        <div className="row">
          {modules.map((m) => (
            <span key={m.path} className={`chip ${m.error ? "bad" : "ok"}`} title={m.error || m.path}>
              {m.error ? "✕" : "✓"} {m.name}
              {m.version && <span className="muted">{m.version}</span>}
              {!m.error && m.drivers.some((d) => d !== m.name) && <span className="muted">· driver={m.drivers.join(", ")}</span>}
            </span>
          ))}
        </div>
      )}
      {failed.map((m) => (
        <span key={m.path} className="warn small">
          {m.name} didn&apos;t load: {m.error}
        </span>
      ))}
      <span className="field-hint">
        One per radio family — macOS: <code>brew install soapyhackrf</code>; Debian/Ubuntu: <code>sudo apt install soapysdr0.8-module-all</code>.
      </span>
    </div>
  );
}

type EditSource = (fn: (x: Source) => void) => void;

/** A number typed as text (kept while typing); blank = undefined. Negative and decimal numbers too. */
function DecInput(props: { value: number | undefined; placeholder?: string; disabled?: boolean; label: string; onChange: (v: number | undefined) => void; narrow?: boolean }) {
  const [text, setText] = useState(props.value === undefined ? "" : String(props.value));
  useEffect(() => setText((t) => (parse(t) === props.value ? t : props.value === undefined ? "" : String(props.value))), [props.value]); // eslint-disable-line react-hooks/exhaustive-deps
  function parse(t: string): number | undefined {
    const v = t.trim().replace(",", ".");
    return v === "" || !Number.isFinite(Number(v)) ? undefined : Number(v);
  }
  return (
    <input
      className={`mono${props.narrow ? " narrow" : ""}`}
      aria-label={props.label}
      value={text}
      disabled={props.disabled}
      placeholder={props.placeholder}
      onChange={(e) => {
        setText(e.target.value);
        props.onChange(parse(e.target.value));
      }}
    />
  );
}

function PpmField(props: { src: Exclude<Source, { type: "file" }>; edit: EditSource }) {
  const { src, edit } = props;
  return (
    <Field label="Frequency correction, ppm" hint={src.type === "rtlsdr" ? "Whole numbers" : undefined}>
      <DecInput
        label="Frequency correction, ppm"
        value={src.ppm}
        onChange={(v) =>
          edit((x) => {
            if (x.type !== "file") x.ppm = x.type === "rtlsdr" ? Math.round(v ?? 0) : v ?? 0;
          })
        }
      />
    </Field>
  );
}

/**
 * AutoTune (Trunk Recorder's): correct the source's channels for the error its
 * control channels show. While recording, what was measured, and the ppm that
 * would remove it.
 */
function AutoTune(props: { src: Source; i: number; edit: EditSource }) {
  const { src, i, edit } = props;
  const st = useApp().sources.find((x) => x.index === i);
  const err = st?.errorPpm ?? null;
  const ppm = src.type === "file" ? null : src.ppm;
  // Measured against the ppm set now: setting ppm − error removes it.
  const better = err !== null && ppm !== null ? (src.type === "rtlsdr" ? Math.round(ppm - err) : Math.round((ppm - err) * 10) / 10) : null;
  return (
    <div className="field wide">
      <Toggle
        label="AutoTune"
        hint="follows crystal drift using the control channel (P25, SmartNet)"
        checked={!!src.autoTune}
        onChange={(v) =>
          edit((x) => {
            if (v) x.autoTune = true;
            else delete x.autoTune;
          })
        }
      />
      {err !== null && (
        <span className="field-hint">
          Measured error: <span className="mono">{err >= 0 ? "+" : ""}{err.toFixed(2)} ppm</span>
          {src.autoTune && st?.tunePpm !== undefined && <> · corrected by <span className="mono">{st.tunePpm >= 0 ? "+" : ""}{st.tunePpm.toFixed(2)} ppm</span></>}
          {better !== null && better !== ppm && Math.abs(err) >= 0.3 && (
            <>
              {" "}
              ·{" "}
              <button className="btn ghost small" onClick={() => edit((x) => x.type !== "file" && void (x.ppm = better))}>
                Set correction to {better} ppm
              </button>
            </>
          )}
        </span>
      )}
    </div>
  );
}

const AIRSPY_MODES: { mode: AirspyGainMode; label: string; hint: string }[] = [
  { mode: "linearity", label: "Linearity", hint: "Best near transmitters" },
  { mode: "sensitivity", label: "Sensitivity", hint: "Best for weak signals" },
  { mode: "manual", label: "Each stage", hint: "LNA, mixer and VGA set separately" },
];

function AirspyGainFields(props: { src: Extract<Source, { type: "airspy" }>; edit: EditSource }) {
  const { src, edit } = props;
  const set = (k: "gainStep" | "lnaStep" | "mixerStep" | "vgaStep", v: number) => edit((x) => x.type === "airspy" && void (x[k] = v));
  const slider = (k: "gainStep" | "lnaStep" | "mixerStep" | "vgaStep", label: string, max: number, disabled = false) => (
    <Field label={label}>
      <div className="row">
        <input type="range" min={0} max={max} value={src[k]} disabled={disabled} aria-label={label} aria-valuetext={String(src[k])} onChange={(e) => set(k, Number(e.target.value))} />
        <span className="mono small">{disabled ? "auto" : src[k]}</span>
      </div>
    </Field>
  );
  return (
    <>
      <Field label="Gain" hint={AIRSPY_MODES.find((m) => m.mode === src.gainMode)?.hint}>
        <select value={src.gainMode} onChange={(e) => edit((x) => x.type === "airspy" && void (x.gainMode = e.target.value as AirspyGainMode))}>
          {AIRSPY_MODES.map((m) => (
            <option key={m.mode} value={m.mode}>
              {m.label}
            </option>
          ))}
        </select>
      </Field>
      {src.gainMode !== "manual" ? (
        slider("gainStep", `${src.gainMode === "linearity" ? "Linearity" : "Sensitivity"} step`, 21)
      ) : (
        <>
          <Toggle label="AGC" hint="LNA and mixer only" checked={src.agc} onChange={(v) => edit((x) => x.type === "airspy" && void (x.agc = v))} />
          {slider("lnaStep", "LNA", 14, src.agc)}
          {slider("mixerStep", "Mixer", 15, src.agc)}
          {slider("vgaStep", "VGA (IF)", 15)}
        </>
      )}
    </>
  );
}

/** Stage names SoapySDR modules commonly have. */
const SOAPY_STAGES = ["LNA", "VGA", "AMP", "MIX", "IF", "BB", "TIA", "PGA", "IFGR", "RFGR", "VGA1", "VGA2", "LNAGR"];

/** A SoapySDR device's gain stages (HackRF LNA / VGA / AMP, SDRplay IFGR / RFGR, LimeSDR LNA / TIA / PGA …), each in dB. */
function GainStages(props: { i: number; gains: Record<string, number>; disabled: boolean; edit: EditSource }) {
  const { i, gains, disabled, edit } = props;
  const list = `stages-${i}`;
  const [name, setName] = useState("");
  const set = (fn: (g: Record<string, number>) => void) =>
    edit((x) => {
      if (x.type !== "soapy") return;
      x.gains = { ...x.gains };
      fn(x.gains);
    });
  const add = () => {
    const n = name.trim().toUpperCase();
    if (!n || n in gains) return;
    set((g) => void (g[n] = 0));
    setName("");
  };
  return (
    <Field label="Gain stages, dB" hint={disabled ? "Not used with AGC on" : "HackRF: LNA 0–40, VGA 0–62, AMP 0 / 14"} wide>
      <div className="row wrap">
        {Object.entries(gains).map(([k, v]) => (
          <span key={k} className="stage">
            <span className="mono small">{k}</span>
            <DecInput narrow label={`${k} gain, dB`} value={v} disabled={disabled} onChange={(n) => set((g) => void (g[k] = n ?? 0))} />
            <button className="btn ghost small danger" aria-label={`Remove the ${k} stage`} title="Remove" onClick={() => set((g) => void delete g[k])}>
              ×
            </button>
          </span>
        ))}
        <input className="mono narrow" list={list} value={name} placeholder="stage" aria-label="Stage name" onChange={(e) => setName(e.target.value)} onKeyDown={(e) => e.key === "Enter" && add()} />
        <datalist id={list}>
          {SOAPY_STAGES.filter((n) => !(n in gains)).map((n) => (
            <option key={n} value={n} />
          ))}
        </datalist>
        <button className="btn ghost small" disabled={!name.trim()} onClick={add}>
          Add stage
        </button>
      </div>
    </Field>
  );
}

const KINDS: { kind: Source["type"]; label: string; desktop?: boolean }[] = [
  { kind: "rtlsdr", label: "RTL-SDR" },
  { kind: "usrp", label: "USRP", desktop: true },
  { kind: "airspy", label: "Airspy", desktop: true },
  { kind: "soapy", label: "SoapySDR", desktop: true },
  { kind: "file", label: "Capture file" },
];

/** `fresh`: just added (measuring its roll-off is offered). */
function SourceCard(props: { c: Config; i: number; fresh?: boolean; onFreshDone?: () => void }) {
  const s = useApp();
  const { c, i } = props;
  const src = c.sources[i];
  const center = resolvedCenters(c)[i];
  const auto = src.centerHz ? null : center;
  const edit = (fn: (x: Source) => void) => updateConfig((x) => fn(x.sources[i]));
  const setKind = (kind: Source["type"]) =>
    updateConfig((x) => {
      const old = x.sources[i];
      const fresh = kind === "rtlsdr" ? newDongle() : kind === "usrp" ? newUsrp() : kind === "airspy" ? newAirspy() : kind === "soapy" ? newSoapy() : newFile();
      x.sources[i] = { ...fresh, centerHz: old.centerHz } as Source;
    });
  const fileRef = useRef<HTMLInputElement>(null);
  const others = c.sources.filter((_, k) => k !== i);
  const used = new Set(others.map((x) => (x.type === "rtlsdr" ? `r:${x.serial}` : x.type === "airspy" ? `a:${x.serial}` : x.type === "usrp" ? `u:${x.args}` : x.type === "soapy" ? `s:${x.args}` : "")));
  const radios = s.radios;
  const needs = useNeed()(`src-${i}`);

  return (
    <div className={`source-card${needs ? " needs" : ""}`} id={`need-src-${i}`}>
      {needs && <div className="field-needs">{needs}</div>}
      <div className="row source-head">
        <strong>Source {i + 1}</strong>
        <div className="seg small" role="radiogroup" aria-label={`Source ${i + 1} kind`}>
          {KINDS.filter((k) => !k.desktop || !web).map((k) => (
            <button key={k.kind} role="radio" aria-checked={src.type === k.kind} className={src.type === k.kind ? "on" : ""} onClick={() => setKind(k.kind)}>
              {k.label}
            </button>
          ))}
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
        {src.type === "rtlsdr" && (
          <Field
            label="Dongle"
            hint={s.devices.length ? undefined : web ? "No dongle connected" : "No dongle found"}
          >
            <div className="row">
              <select value={src.serial} onChange={(e) => edit((x) => x.type === "rtlsdr" && void (x.serial = e.target.value))}>
                <option value="">First available</option>
                {s.devices.map((d) => (
                  <option key={d.serial || d.index} value={d.serial} disabled={used.has(`r:${d.serial}`)}>
                    {d.product}
                    {d.serial ? ` · SN ${d.serial}` : ""}
                    {used.has(`r:${d.serial}`) ? " (in use)" : ""}
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
        )}
        {src.type === "usrp" &&
          (radios && !radios.usrp.available ? (
            <DriverMissing kind="usrp" detail={radios.usrp.detail} />
          ) : (
            <Field
              label="USRP"
              hint={
                radios?.usrp.devices === null
                  ? radios?.usrp.detail ?? "UHD"
                  : radios?.usrp.devices?.length
                    ? radios.usrp.detail
                    : "None found; device arguments, e.g. addr=192.168.10.2"
              }
            >
              <div className="row">
                <input
                  className="mono"
                  list={`usrp-${i}`}
                  value={src.args}
                  placeholder="first found"
                  onChange={(e) => edit((x) => x.type === "usrp" && void (x.args = e.target.value))}
                />
                <datalist id={`usrp-${i}`}>
                  {(radios?.usrp.devices ?? []).map((d) => (
                    <option key={d.args} value={d.args}>
                      {d.label}
                    </option>
                  ))}
                </datalist>
                <button className="btn ghost small" disabled={s.findingRadios} onClick={findRadios}>
                  {s.findingRadios ? "Searching…" : "Find"}
                </button>
              </div>
            </Field>
          ))}
        {src.type === "airspy" &&
          (radios && !radios.airspy.available ? (
            <DriverMissing kind="airspy" detail={radios.airspy.detail} />
          ) : (
            <Field label="Airspy" hint={radios?.airspy.devices?.length ? radios.airspy.detail : `${radios?.airspy.detail ?? "libairspy"} · none found`}>
              <div className="row">
                <select value={src.serial} onChange={(e) => edit((x) => x.type === "airspy" && void (x.serial = e.target.value))}>
                  <option value="">First available</option>
                  {src.serial && !(radios?.airspy.devices ?? []).some((d) => d.serial === src.serial) && <option value={src.serial}>SN {src.serial} (not connected)</option>}
                  {(radios?.airspy.devices ?? []).map((d) => (
                    <option key={d.serial} value={d.serial} disabled={used.has(`a:${d.serial}`)}>
                      {d.label}
                      {used.has(`a:${d.serial}`) ? " (in use)" : ""}
                    </option>
                  ))}
                </select>
                <button className="btn ghost small" disabled={s.findingRadios} onClick={findRadios}>
                  Refresh
                </button>
              </div>
            </Field>
          ))}
        {src.type === "soapy" &&
          (radios?.soapy && !radios.soapy.available ? (
            <DriverMissing kind="soapy" detail={radios.soapy.detail} />
          ) : (
            <>
              <Field
                label="Device"
                hint={
                  !radios?.soapy || radios.soapy.devices === null
                    ? radios?.soapy?.detail ?? "SoapySDR"
                    : radios.soapy.devices.length
                      ? radios.soapy.detail
                      : "None found; device arguments, e.g. driver=hackrf"
                }
              >
                <div className="row">
                  <input
                    className="mono"
                    list={`soapy-${i}`}
                    value={src.args}
                    placeholder="first found"
                    onChange={(e) => edit((x) => x.type === "soapy" && void (x.args = e.target.value))}
                  />
                  <datalist id={`soapy-${i}`}>
                    {(radios?.soapy?.devices ?? []).map((d) => (
                      <option key={d.args} value={d.args} disabled={used.has(`s:${d.args}`)}>
                        {d.label}
                        {used.has(`s:${d.args}`) ? " (in use)" : ""}
                      </option>
                    ))}
                  </datalist>
                  <button className="btn ghost small" disabled={s.findingRadios} onClick={findRadios}>
                    {s.findingRadios ? "Searching…" : "Find"}
                  </button>
                </div>
              </Field>
              {radios?.soapy && <SoapyModules soapy={radios.soapy} />}
            </>
          ))}
        {src.type === "file" &&
          (web ? (
            <Field label="Capture file" hint="rtl_sdr output (8-bit IQ); forgotten on reload" wide>
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
                  edit((x) => x.type === "file" && void (x.path = f?.name ?? ""));
                  e.target.value = "";
                }}
              />
            </Field>
          ) : (
            <Field label="Capture file" wide>
              <input
                className="mono"
                value={src.path}
                placeholder="/path/to/capture.cu8"
                onChange={(e) =>
                  edit((x) => {
                    if (x.type !== "file") return;
                    x.path = e.target.value;
                    x.format = formatFromPath(x.path);
                  })
                }
              />
            </Field>
          ))}
        <Field
          label="Center frequency, MHz"
          hint={src.centerHz ? undefined : auto ? `Auto: ${formatMhz(auto, 4)} MHz` : "Auto: no uncovered system to place over"}
        >
          <MhzInput hz={src.centerHz} placeholder={auto ? formatMhz(auto, 4) : "MHz"} onChange={(hz) => edit((x) => void (x.centerHz = hz))} />
        </Field>
        <Field
          label="Sample rate"
          hint={src.type === "usrp" ? "MSPS; wider costs more CPU" : src.type === "airspy" ? "R2: 10 or 2.5; Mini: 6 or 3" : src.type === "soapy" ? "MSPS, one the device supports" : undefined}
        >
          <RateInput
            key={src.type}
            hz={src.rateHz}
            free={src.type === "usrp" || src.type === "soapy" || (src.type === "file" && !web)}
            options={src.type === "usrp" ? USRP_RATES : src.type === "airspy" ? AIRSPY_RATES : src.type === "soapy" ? SOAPY_RATES : src.type === "file" ? [...SAMPLE_RATES, 8_000_000, 10_000_000] : SAMPLE_RATES}
            onChange={(hz) => edit((x) => void (x.rateHz = hz))}
          />
        </Field>
        {src.type === "rtlsdr" && (
          <>
            <Field label="Gain, dB" hint={src.agc ? "A fixed gain usually works better" : "Most dongles: 0–49.6"}>
              <div className="row">
                <GainInput label="Gain, dB" value={src.gainDb} disabled={src.agc} onChange={(v) => v !== null && edit((x) => x.type === "rtlsdr" && void (x.gainDb = v))} />
                <Toggle label="AGC" checked={src.agc} onChange={(v) => edit((x) => x.type === "rtlsdr" && void (x.agc = v))} />
              </div>
            </Field>
            <PpmField src={src} edit={edit} />
          </>
        )}
        {src.type === "usrp" && (
          <>
            <Field label="Gain, dB" hint={src.agc ? "AGC: B200 / B210 and E3xx only" : "B200/B210: 0–76"}>
              <div className="row">
                <GainInput label="Gain, dB" value={src.gainDb} disabled={src.agc} onChange={(v) => edit((x) => x.type === "usrp" && void (x.gainDb = v || 0))} />
                <Toggle label="AGC" checked={src.agc} onChange={(v) => edit((x) => x.type === "usrp" && void (x.agc = v))} />
              </div>
            </Field>
            <Field label="Antenna" hint="e.g. RX2, TX/RX">
              <input className="mono" value={src.antenna} placeholder="default" onChange={(e) => edit((x) => x.type === "usrp" && void (x.antenna = e.target.value.trim()))} />
            </Field>
            <PpmField src={src} edit={edit} />
          </>
        )}
        {src.type === "airspy" && <AirspyGainFields src={src} edit={edit} />}
        {src.type === "airspy" && (
          <>
            <PpmField src={src} edit={edit} />
            <Toggle label="Bias-T" hint="powers an LNA via the antenna cable" checked={src.biasTee} onChange={(v) => edit((x) => x.type === "airspy" && void (x.biasTee = v))} />
          </>
        )}
        {src.type === "soapy" && (
          <>
            <Field label="Gain, dB">
              <div className="row">
                <GainInput label="Gain, dB" value={src.gainDb} placeholder={src.agc ? "AGC" : "as is"} disabled={src.agc} onChange={(v) => edit((x) => x.type === "soapy" && void (x.gainDb = v))} />
                <Toggle label="AGC" checked={src.agc} onChange={(v) => edit((x) => x.type === "soapy" && void (x.agc = v))} />
              </div>
            </Field>
            <Field label="Antenna">
              <input className="mono" value={src.antenna} placeholder="default" onChange={(e) => edit((x) => x.type === "soapy" && void (x.antenna = e.target.value.trim()))} />
            </Field>
            <GainStages i={i} gains={src.gains} disabled={src.agc} edit={edit} />
            <Field label="Device settings" hint="key=value, e.g. biastee=true (see SoapySDRUtil --probe)">
              <input className="mono" value={src.settings} placeholder="none" onChange={(e) => edit((x) => x.type === "soapy" && void (x.settings = e.target.value))} />
            </Field>
            <PpmField src={src} edit={edit} />
          </>
        )}
        {src.type === "file" && (
          <>
            {!web && (
              <Field label="Sample format">
                <select value={src.format ?? "cu8"} onChange={(e) => edit((x) => x.type === "file" && void (x.format = e.target.value as "cu8" | "cs16" | "cf32"))}>
                  <option value="cu8">cu8 — rtl_sdr (unsigned 8-bit)</option>
                  <option value="cs16">cs16 — signed 16-bit</option>
                  <option value="cf32">cf32 — float (GNU Radio, UHD)</option>
                </select>
              </Field>
            )}
            <Toggle label="Real-time pace" hint="off = as fast as it decodes" checked={src.realtime} onChange={(v) => edit((x) => x.type === "file" && void (x.realtime = v))} />
          </>
        )}
        <AutoTune src={src} i={i} edit={edit} />
        <GuardBand c={c} i={i} fresh={props.fresh} onFreshDone={props.onFreshDone} />
      </div>
      {center ? <CoverageBar c={c} center={center} rateHz={src.rateHz} guardHz={src.guardHz} /> : null}
    </div>
  );
}

/** A source's band, with every system's control (tall) and known voice channels (short) inside it. */
function CoverageBar(props: { c: Config; center: number; rateHz: number; guardHz?: number }) {
  const { c, center, rateHz } = props;
  const half = usableHalfWidth(rateHz, props.guardHz);
  const lo = center - rateHz / 2;
  const pos = (hz: number) => ((hz - lo) / rateHz) * 100;
  const inside = (hz: number) => Math.abs(hz - center) <= half;
  const systems = activeSystems(c);
  const ticks: { hz: number; kind: "cc" | "voice"; color: string; label: string }[] = [];
  systems.forEach((x) => {
    for (const f of x.controlChannelsHz) if (inside(f)) ticks.push({ hz: f, kind: "cc", color: systemColor(c, x.shortName), label: `${x.shortName} control channel ${formatMhz(f)} MHz` });
    for (const f of x.voiceChannelsHz) if (inside(f)) ticks.push({ hz: f, kind: "voice", color: systemColor(c, x.shortName), label: `${x.shortName} voice ${formatMhz(f)} MHz` });
  });
  const conv = enabledChannels(c).filter((ch) => inside(ch.freqHz));
  for (const ch of conv) ticks.push({ hz: ch.freqHz, kind: "voice", color: "var(--text)", label: `conventional ${ch.name || formatMhz(ch.freqHz)}` });
  const here = systems.filter((x) => x.controlChannelsHz.some(inside) || x.voiceChannelsHz.some(inside));
  return (
    <div className="stack" style={{ gap: 4 }}>
      <div className="coverage" role="img" aria-label={`Covers ${formatMhz(center - half, 3)} to ${formatMhz(center + half, 3)} MHz`}>
        <div className="usable" style={{ left: `${pos(center - half)}%`, width: `${(2 * half * 100) / rateHz}%` }} />
        {ticks.map((t, k) => (
          <span key={k} className={`tick ${t.kind}`} style={{ left: `${pos(t.hz)}%`, background: t.color }} title={t.label} />
        ))}
      </div>
      <div className="legend">
        <span className="mono">
          {formatMhz(center - half, 3)} – {formatMhz(center + half, 3)} MHz
        </span>
        {here.map((x) => (
          <span key={x.shortName}>
            <span className="sys-dot" style={{ background: systemColor(c, x.shortName) }} />
            {x.shortName}
          </span>
        ))}
        {conv.length > 0 && <span>{conv.length} conventional</span>}
        {!here.length && !conv.length && <span>nothing in range</span>}
      </div>
    </div>
  );
}

let nextRowId = 1;

/**
 * The Conventional tab: each conventional system — its own short name,
 * channels (or channel file), squelch, names, uploads and recording rules.
 */
function ConventionalTab(props: { c: Config }) {
  const { c } = props;
  const s = useApp();
  return (
    <>
      {c.conventional.map((_, k) => (
        <ConventionalPanel key={`${k}-${c.conventional.length}-${s.configEpoch}`} c={c} k={k} />
      ))}
      <section className="panel">
        <div className="row">
          <p className="muted small grow">
            {c.conventional.length ? null : "No conventional systems yet."}
          </p>
          <button className="btn" onClick={() => updateConfig((x) => void x.conventional.push(newConventional(x)))}>
            Add a conventional system
          </button>
        </div>
      </section>
    </>
  );
}

/**
 * One conventional system: a table editor, bulk add, CSV import / export,
 * and (desktop) a linked CSV file to edit in a spreadsheet instead.
 */
function ConventionalPanel(props: { c: Config; k: number }) {
  const { c, k } = props;
  const need = useNeed();
  const plugins = useApp().plugins?.plugins ?? [];
  const conv = c.conventional[k];
  const chans = conv.channels;
  const name = conv.shortName;
  const dupName = c.conventional.some((x, j) => j !== k && x.shortName === name);
  // Stable row keys (each edit clones the config; a frequency input keeps its own text).
  const ids = useRef<number[]>([]);
  if (ids.current.length !== chans.length) ids.current = chans.map(() => nextRowId++);
  const csvRef = useRef<HTMLInputElement>(null);
  const [bulk, setBulk] = useState("");
  const [pending, setPending] = useState<{ name: string; channels: Channel[]; notes: string[] } | null>(null);
  const [filePath, setFilePath] = useState(`${conv.shortName || "conv"}-channels.csv`);
  const linked = !web && !!conv.channelFile;
  const [bulkMode, setBulkMode] = useState<Channel["mode"]>("fm");
  const edit = (fn: (x: Conventional) => void) =>
    updateConfig((x) => {
      fn(x.conventional[k]);
    });
  const editRow = (i: number, fn: (ch: Channel) => void) => edit((x) => fn(x.channels[i]));
  const remove = (i: number) => {
    ids.current.splice(i, 1);
    edit((x) => void x.channels.splice(i, 1));
  };
  const heard = useApp().heard;
  /** The last row on row i's frequency. */
  const lastOfFreq = (i: number) => chans.reduce((at, o, k) => (Math.abs(o.freqHz - chans[i].freqHz) < 1 ? Math.max(at, k) : at), i);
  /** Another row on row i's frequency (after the last of them), for another tone — `tone` when it is known. */
  const addTone = (i: number, tone = "") => {
    const ch = chans[i];
    const at = lastOfFreq(i);
    const row: Channel = { freqHz: ch.freqHz, mode: ch.mode, name: "", enabled: true, tone };
    // A DMR / NXDN code naming a talkgroup files under it; others get the next free number.
    if (!(ch.mode !== "fm" && ch.mode !== "p25" && /\bTG \d/.test(tone))) row.talkgroup = nextTalkgroup(chans, ch.freqHz);
    if (!tone) delete row.tone;
    if (ch.squelchDb !== undefined) row.squelchDb = ch.squelchDb;
    ids.current.splice(at + 1, 0, nextRowId++);
    edit((x) => void x.channels.splice(at + 1, 0, row));
  };
  const talkgroups = channelTalkgroups(chans);
  const shared = (i: number) => chans.some((o, k) => k !== i && Math.abs(o.freqHz - chans[i].freqHz) < 1);
  const add = (list: Channel[]) => {
    ids.current.push(...list.map(() => nextRowId++));
    edit((x) => void x.channels.push(...list));
  };
  const replaceAll = (list: Channel[]) => {
    ids.current = list.map(() => nextRowId++);
    edit((x) => void (x.channels = list));
  };
  const onCsv = async (f: File | undefined) => {
    if (!f) return;
    const { channels, notes, error } = parseChannelCsv(await f.text());
    if (error) return setNotice(`${f.name}: ${error}`);
    if (!chans.length) {
      replaceAll(channels);
      setNotice(`Read ${channels.length} channel(s) from ${f.name}.${notes.length ? " " + notes.join(" ") : ""}`);
    } else setPending({ name: f.name, channels, notes });
  };
  const exportCsv = () => downloadText(`${conv.shortName || "channels"}-channels.csv`, channelsToCsv(chans));
  const enabled = chans.filter((ch) => ch.enabled).length;
  const optNum = (v: string): number | undefined => (v.trim() === "" || !Number.isFinite(Number(v)) ? undefined : Number(v));

  return (
    <section className={`panel${conv.enabled ? "" : " off"}`} id={`need-conv-${name}`}>
      <header className="panel-head">
        <h2>
          {conv.name?.trim() || name || "(no name)"} <span className="muted small">{chans.length} channel{chans.length === 1 ? "" : "s"}</span>
        </h2>
        <div className="row">
          <label className="toggle small">
            <input type="checkbox" checked={conv.enabled} onChange={(e) => edit((x) => void (x.enabled = e.target.checked))} />
            <span>Record</span>
          </label>
          <button
            className="btn ghost"
            onClick={() => {
              if (window.confirm(`Remove ${name || "this conventional system"} and its ${chans.length} channel${chans.length === 1 ? "" : "s"}? Its recordings stay on disk.`))
                updateConfig((x) => void x.conventional.splice(k, 1));
            }}
          >
            Remove
          </button>
          {!linked && (
            <button className="btn ghost" onClick={() => csvRef.current?.click()}>
              Import CSV…
            </button>
          )}
          <button className="btn ghost" disabled={!chans.length} onClick={exportCsv}>
            Export CSV
          </button>
          {!linked && (
            <button className="btn ghost" onClick={() => add([{ freqHz: 0, mode: bulkMode, name: "", enabled: true }])}>
              Add a channel
            </button>
          )}
        </div>
        <input
          ref={csvRef}
          type="file"
          accept=".csv,text/csv"
          hidden
          onChange={(e) => {
            void onCsv(e.target.files?.[0]);
            e.target.value = "";
          }}
        />
      </header>
      <div className="stack">
        {pending && (
          <div className="banner">
            <div>
              Read {pending.channels.length} channel{pending.channels.length === 1 ? "" : "s"} from {pending.name}.{pending.notes.length ? " " + pending.notes.join(" ") : ""}
            </div>
            <div className="row">
              <button
                className="btn primary"
                onClick={() => {
                  replaceAll(pending.channels);
                  setPending(null);
                }}
              >
                Replace the list ({chans.length} now)
              </button>
              <button
                className="btn"
                onClick={() => {
                  add(pending.channels);
                  setPending(null);
                }}
              >
                Add to the list
              </button>
              <button className="btn ghost" onClick={() => setPending(null)}>
                Cancel
              </button>
            </div>
          </div>
        )}
        {!web &&
          (linked ? (
            <div className="banner">
              <div>
                Channels come from <code>{conv.channelFile}</code>, re-read each time recording starts.
              </div>
              {conv.channelFileStatus && <div className="small mono">{conv.channelFileStatus}</div>}
              <div className="row">
                <button className="btn" onClick={() => setChannelFile(k, conv.channelFile ?? "")}>
                  Reload
                </button>
                <button className="btn ghost" onClick={() => setChannelFile(k, "")} title="Stop reading the file; keep the channels here">
                  Unlink
                </button>
              </div>
            </div>
          ) : (
            <Field
              needs={need(`channels-${name}`)}
              anchor={`channels-${name}`}
              label="Channel file (optional)"
              hint="A CSV to edit in a spreadsheet; created from this list if new"
              wide
            >
              <div className="row">
                <input className="mono grow" value={filePath} placeholder="channels.csv" onChange={(e) => setFilePath(e.target.value)} />
                <button className="btn" disabled={!filePath.trim()} onClick={() => setChannelFile(k, filePath.trim())}>
                  Use this file
                </button>
              </div>
            </Field>
          ))}
        <div className="grid3">
          <Field label="Short name" hint={dupName ? "Already used — each needs its own folder" : "Its calls' folder name"}>
            <input
              value={conv.shortName}
              onChange={(e) => {
                const to = e.target.value.replace(/[^\w.-]/g, "");
                updateConfig((x) => {
                  renameSystemRefs(x, x.conventional[k].shortName, to, plugins);
                  x.conventional[k].shortName = to;
                });
              }}
            />
          </Field>
          <Field label="Name">
            <input value={conv.name ?? ""} placeholder="County Fire" onChange={(e) => edit((x) => void (e.target.value ? (x.name = e.target.value) : delete x.name))} />
          </Field>
          <Field needs={need(`squelch-${name}`)} anchor={`squelch-${name}`} label="Squelch, dB above noise" hint="For channels without their own">
            <input className="mono" value={conv.squelchDb} onChange={(e) => edit((x) => void (x.squelchDb = Math.max(3, Math.min(40, Number(e.target.value) || 8))))} />
          </Field>
          {!linked && (
            <Field label="Add frequencies, MHz" wide>
              <div className="row">
                <input className="mono grow" value={bulk} placeholder="154.430, 155.100, 460.125" onChange={(e) => setBulk(e.target.value)} />
                <select value={bulkMode} onChange={(e) => setBulkMode(e.target.value as Channel["mode"])} aria-label="Mode for the added channels">
                  <option value="fm">Analog FM</option>
                  <option value="p25">P25</option>
                  <option value="dmr">DMR</option>
                  <option value="nxdn48">NXDN48 (6.25 kHz)</option>
                  <option value="nxdn96">NXDN96 (12.5 kHz)</option>
                </select>
                <button
                  className="btn"
                  disabled={!parseFreqList(bulk).length}
                  onClick={() => {
                    add(parseFreqList(bulk).map((freqHz) => ({ freqHz, mode: bulkMode, name: "", enabled: true })));
                    setBulk("");
                  }}
                >
                  Add
                </button>
              </div>
            </Field>
          )}
        </div>
        {chans.length > 0 && (
          <>
            <div className="table-wrap channels-wrap">
              <table className="calls channels">
                <thead>
                  <tr>
                    <th title="Record this channel">On</th>
                    <th>Frequency, MHz</th>
                    <th>Mode</th>
                    <th title="Tone, NAC, colour code or RAN to record; blank takes the rest">
                      Tone
                    </th>
                    <th>Name</th>
                    <th title="Calls are filed under it; P25 uses the on-air one">Talkgroup</th>
                    <th title="dB above the noise floor">Squelch</th>
                    <th />
                  </tr>
                </thead>
                <tbody>
                  {chans.map((ch, i) => (
                    <Fragment key={ids.current[i]}>
                    <tr className={`${ch.enabled ? "" : "st-monitoring"}${shared(i) ? " shared-freq" : ""}`}>
                      <td>
                        <input type="checkbox" disabled={linked} checked={ch.enabled} aria-label="Record this channel" onChange={(e) => editRow(i, (x) => void (x.enabled = e.target.checked))} />
                      </td>
                      <td>
                        {linked ? <span className="mono">{mhzCell(ch.freqHz)}</span> : <MhzInput hz={ch.freqHz} onChange={(hz) => editRow(i, (x) => void (x.freqHz = hz))} />}
                      </td>
                      <td>
                        <select value={ch.mode} disabled={linked} aria-label="Mode" onChange={(e) => editRow(i, (x) => void (x.mode = e.target.value as Channel["mode"]))}>
                          <option value="fm">Analog FM</option>
                          <option value="p25">P25</option>
                          <option value="dmr">DMR</option>
                          <option value="nxdn48">NXDN48 (6.25 kHz)</option>
                          <option value="nxdn96">NXDN96 (12.5 kHz)</option>
                        </select>
                      </td>
                      <td>
                        <ToneInput
                          key={ch.mode}
                          mode={ch.mode}
                          tone={ch.tone ?? ""}
                          disabled={linked}
                          onChange={(t) =>
                            editRow(i, (x) => {
                              if (t) x.tone = t;
                              else delete x.tone;
                            })
                          }
                        />
                      </td>
                      <td>
                        <input value={ch.name} disabled={linked} placeholder="Name" aria-label="Name" onChange={(e) => editRow(i, (x) => void (x.name = e.target.value))} />
                      </td>
                      <td>
                        <input
                          className="mono narrow"
                          disabled={linked}
                          value={ch.talkgroup ?? ""}
                          placeholder={ch.freqHz ? String(talkgroups[i]) : "auto"}
                          aria-label="Talkgroup"
                          onChange={(e) =>
                            editRow(i, (x) => {
                              const v = optNum(e.target.value);
                              if (v === undefined || v <= 0) delete x.talkgroup;
                              else x.talkgroup = Math.round(v);
                            })
                          }
                        />
                      </td>
                      <td>
                        <input
                          className="mono narrow"
                          disabled={linked}
                          value={ch.squelchDb ?? ""}
                          placeholder={String(conv.squelchDb)}
                          aria-label="Squelch, dB above noise"
                          onChange={(e) =>
                            editRow(i, (x) => {
                              const v = optNum(e.target.value);
                              if (v === undefined) delete x.squelchDb;
                              else x.squelchDb = Math.max(3, Math.min(40, v));
                            })
                          }
                        />
                      </td>
                      <td>
                        {!linked && (
                          <span className="row-actions">
                            {ch.freqHz > 0 && (
                              <button
                                className="btn ghost small"
                                title={`Another ${{ fm: "tone", p25: "NAC", dmr: "colour code or talkgroup", nxdn48: "RAN or talkgroup", nxdn96: "RAN or talkgroup" }[ch.mode]} on this frequency`}
                                aria-label="Add a row on this frequency"
                                onClick={() => addTone(i)}
                              >
                                {{ fm: "+ tone", p25: "+ NAC", dmr: "+ code", nxdn48: "+ code", nxdn96: "+ code" }[ch.mode]}
                              </button>
                            )}
                            <button className="btn ghost small danger" title="Remove" aria-label="Remove channel" onClick={() => remove(i)}>
                              ×
                            </button>
                          </span>
                        )}
                        </td>
                      </tr>
                      {lastOfFreq(i) === i && heard[String(Math.round(ch.freqHz))] && (
                        <HeardRow
                          rows={chans.filter((o) => Math.abs(o.freqHz - ch.freqHz) < 1)}
                          heard={heard[String(Math.round(ch.freqHz))]}
                          linked={linked}
                          onAdd={(code) => addTone(i, code)}
                        />
                      )}
                    </Fragment>
                    ))}
                  </tbody>
                </table>
              </div>
              <div className="row">
                <span className="muted small">
                  {chans.length} channel{chans.length === 1 ? "" : "s"}, {enabled} on
                </span>
                <span className="spacer" />
                {!linked && (
                <button
                  className="btn ghost small danger"
                  onClick={() => {
                    if (window.confirm(`Remove all ${chans.length} conventional channels?`)) {
                      ids.current = [];
                      edit((x) => void (x.channels = []));
                    }
                  }}
                >
                  Remove all
                </button>
              )}
            </div>
          </>
        )}
        {chans.length > 0 && (
          <div className="grid2">
            <UnitNamesField
              value={conv.unitNames}
              anchor={`convunits-${name}`}
              needs={need(`convunits-${name}`)}
              onChange={(u) =>
                edit((x) => {
                  if (u) x.unitNames = u;
                  else delete x.unitNames;
                })
              }
            />
          </div>
        )}
        {chans.length > 0 && <SystemPluginSettings system={{ conv: k }} />}
        {chans.length > 0 && (
          <RecordingOverridePanel
            c={c}
            value={conv.recording}
            onChange={(fn) =>
              edit((x) => {
                x.recording = { ...x.recording };
                fn(x.recording);
                if (!Object.keys(x.recording).length) delete x.recording;
              })
            }
          />
        )}
        <details className="help">
          <summary>CSV format</summary>
          <p className="small">
            A header row, then one channel per row; <b>Export CSV</b> writes one to start from. Columns, in any order: <code>TG Number</code> (empty = the
            frequency in kHz), <code>Frequency</code> (MHz with a decimal point, or Hz), <code>Mode</code> (<code>fm</code> or <code>p25</code>; empty = fm),{" "}
            <code>Alpha Tag</code>, <code>Description</code>, <code>Tag</code>, <code>Category</code>, <code>Squelch dB</code> (above the noise; empty = the
            default), <code>Enable</code> (<code>false</code> = off). Trunk Recorder's channel file reads as is; its <code>Squelch</code> column (an absolute
            level) isn't used.
          </p>
        </details>
      </div>
    </section>
  );
}

/** A hex or decimal number field; empty = none. */
function NumInput(props: { value: number | null | undefined; hex?: boolean; placeholder?: string; onChange: (v: number | null) => void; label: string }) {
  const show = (v: number | null | undefined) => (v === null || v === undefined ? "" : props.hex ? v.toString(16).toUpperCase() : String(v));
  const [text, setText] = useState(show(props.value));
  useEffect(() => setText((t) => (parse(t) === (props.value ?? null) ? t : show(props.value))), [props.value]); // eslint-disable-line react-hooks/exhaustive-deps
  function parse(t: string): number | null {
    const v = t.trim();
    if (!v) return null;
    const n = props.hex ? parseInt(v, 16) : Number(v);
    return Number.isFinite(n) && n >= 0 ? n : null;
  }
  return (
    <input
      className="mono"
      aria-label={props.label}
      value={text}
      placeholder={props.placeholder ?? "any"}
      onChange={(e) => {
        setText(e.target.value);
        props.onChange(parse(e.target.value));
      }}
    />
  );
}

const hexText = (v: number | null | undefined) => (v === null || v === undefined ? "?" : v.toString(16).toUpperCase());

/** "WACN BEE00 · SysID 445 · site 1-3" from what is known. */
/** The site-lock inputs, in order. */
const LOCK_INPUTS: { key: keyof SiteIdentity; label: string; hex: boolean }[] = [
  { key: "nac", label: "NAC", hex: true },
  { key: "wacn", label: "WACN", hex: true },
  { key: "sysId", label: "System ID", hex: true },
  { key: "rfss", label: "RFSS", hex: false },
  { key: "site", label: "Site", hex: false },
];

export function siteText(id: SiteIdentity): string {
  const parts: string[] = [];
  if (id.wacn != null) parts.push(`WACN ${hexText(id.wacn)}`);
  if (id.sysId != null) parts.push(`SysID ${hexText(id.sysId)}`);
  if (id.nac != null) parts.push(`NAC ${hexText(id.nac)}`);
  if (id.site != null) parts.push(id.rfss != null ? `site ${id.rfss}-${id.site}` : `site ${id.site}`);
  else if (id.rfss != null) parts.push(`RFSS ${id.rfss}`);
  return parts.join(" · ");
}

/** A SmartNet system's band plan (Trunk Recorder's settings) and default voice mode. */
/** "101=452.275, 102=452.3" ↔ { "101": 452275000, … }. */
function lcnText(t: Record<string, number> | undefined): string {
  return Object.entries(t ?? {})
    .map(([k, v]) => `${k}=${(v / 1e6).toFixed(5).replace(/0+$/, "")}`)
    .join(", ");
}
function parseLcn(s: string): Record<string, number> {
  const t: Record<string, number> = {};
  for (const part of s.split(/[,;\s]+/)) {
    const m = /^(\d+)=(\d+(?:\.\d+)?)$/.exec(part.trim());
    if (m) {
      const v = Number(m[2]);
      t[m[1]] = v < 1e5 ? Math.round(v * 1e6) : v;
    }
  }
  return t;
}

function DmrFields(props: { sys: System; edit: (fn: (x: System) => void) => void }) {
  const { sys, edit } = props;
  const [chText, setChText] = useState(() => (sys.dmrChannelsHz ?? []).map((f) => (f / 1e6).toFixed(5)).join(", "));
  const [lcn, setLcn] = useState(() => lcnText(sys.lcnTableHz));
  return (
    <>
      <Field label="Voice frequencies, MHz" hint="Tier III, Capacity Max, Connect Plus" wide>
        <input
          className="mono"
          value={chText}
          placeholder="452.275, 452.300"
          onChange={(e) => {
            setChText(e.target.value);
            const list = parseFreqList(e.target.value);
            edit((x) => {
              if (list.length) x.dmrChannelsHz = list;
              else delete x.dmrChannelsHz;
            });
          }}
        />
      </Field>
      <Field label="Colour code">
        <input
          className="mono"
          value={sys.colorCode ?? ""}
          placeholder="auto"
          onChange={(e) =>
            edit((x) => {
              const v = parseInt(e.target.value, 10);
              if (v >= 0 && v <= 15) x.colorCode = v;
              else delete x.colorCode;
            })
          }
        />
      </Field>
      <Field label="Channel table (optional)" hint="Channel = MHz, e.g. 101=452.275" wide>
        <input
          className="mono"
          value={lcn}
          placeholder="learned from the air"
          onChange={(e) => {
            setLcn(e.target.value);
            const t = parseLcn(e.target.value);
            edit((x) => {
              if (Object.keys(t).length) x.lcnTableHz = t;
              else delete x.lcnTableHz;
            });
          }}
        />
      </Field>
    </>
  );
}

function NxdnFields(props: { sys: System; edit: (fn: (x: System) => void) => void }) {
  const { sys, edit } = props;
  const typeD = sys.nxdnType === "typeD";
  const [chText, setChText] = useState(() => (sys.nxdnChannelsHz ?? []).map((f) => (f / 1e6).toFixed(5)).join(", "));
  const [table, setTable] = useState(() => lcnText(sys.lcnTableHz));
  return (
    <>
      <Field label="NXDN type">
        <select
          value={typeD ? "typeD" : "typeC"}
          onChange={(e) =>
            edit((x) => {
              if (e.target.value === "typeD") x.nxdnType = "typeD";
              else delete x.nxdnType;
            })
          }
        >
          <option value="typeC">Type-C (control channel)</option>
          <option value="typeD">Type-D / IDAS (no control channel)</option>
        </select>
      </Field>
      <Field label="Rate" hint={typeD ? "The repeaters'" : "The control channel's"}>
        <select
          value={sys.nxdnRate === "nxdn96" ? "nxdn96" : "nxdn48"}
          onChange={(e) =>
            edit((x) => {
              if (e.target.value === "nxdn96") x.nxdnRate = "nxdn96";
              else delete x.nxdnRate;
            })
          }
        >
          <option value="nxdn48">NXDN48 (6.25 kHz)</option>
          <option value="nxdn96">NXDN96 (12.5 kHz)</option>
        </select>
      </Field>
      <Field label="RAN">
        <input
          className="mono"
          value={sys.ran ?? ""}
          placeholder="auto"
          onChange={(e) =>
            edit((x) => {
              const v = parseInt(e.target.value, 10);
              if (v >= 0 && v <= 63) x.ran = v;
              else delete x.ran;
            })
          }
        />
      </Field>
      <Field label={typeD ? "More repeaters, MHz" : "Voice frequencies, MHz"} hint={typeD ? "Optional: may all go above" : "Watched; channel numbers are learned from them"} wide>
        <input
          className="mono"
          value={chText}
          placeholder="451.0125, 451.0375"
          onChange={(e) => {
            setChText(e.target.value);
            const list = parseFreqList(e.target.value);
            edit((x) => {
              if (list.length) x.nxdnChannelsHz = list;
              else delete x.nxdnChannelsHz;
            });
          }}
        />
      </Field>
      <Field label="Channel table (optional)" hint={typeD ? "Repeater number = MHz, e.g. 1=451.0125" : "Channel number = MHz, e.g. 20=451.0125"} wide>
        <input
          className="mono"
          value={table}
          placeholder={typeD ? "none" : "learned from the air"}
          onChange={(e) => {
            setTable(e.target.value);
            const t = parseLcn(e.target.value);
            edit((x) => {
              if (Object.keys(t).length) x.lcnTableHz = t;
              else delete x.lcnTableHz;
            });
          }}
        />
      </Field>
      {!typeD && (
        <p className="muted small" style={{ gridColumn: "1 / -1", margin: 0 }}>
          A Type-C control channel grants channel numbers. Their frequencies come from the channel table (RadioReference lists it for many systems), from the site if it
          uses Direct Frequency Assignment, or are learned when the voice frequencies are listed above.
        </p>
      )}
    </>
  );
}

function SmartnetFields(props: { sys: System; edit: (fn: (x: System) => void) => void }) {
  const { sys, edit } = props;
  const custom = (sys.bandplan ?? "").startsWith("400") || sys.bandplan?.toLowerCase() === "obt";
  const setNum = (k: "bandplanBaseHz" | "bandplanSpacingHz" | "bandplanOffset" | "bandplanHighHz", v: number | null) =>
    edit((x) => {
      if (v === null) delete x[k];
      else x[k] = v;
    });
  return (
    <>
      <Field label="Band plan">
        <select value={custom ? "400_custom" : (sys.bandplan ?? "800_standard")} onChange={(e) => edit((x) => void (x.bandplan = e.target.value))}>
          <option value="800_standard">800 MHz standard</option>
          <option value="800_reband">800 MHz rebanded</option>
          <option value="800_splinter">800 MHz splinter</option>
          <option value="900">900 MHz</option>
          <option value="400_custom">VHF / UHF (custom, OBT)</option>
        </select>
      </Field>
      <Field label="Voice of unknown talkgroups" hint="Until a grant says otherwise">
        <select value={sys.defaultMode ?? "digital"} onChange={(e) => edit((x) => void (e.target.value === "analog" ? (x.defaultMode = "analog") : delete x.defaultMode))}>
          <option value="digital">P25</option>
          <option value="analog">Analog FM</option>
        </select>
      </Field>
      {custom && (
        <div className="id-grid wide">
          <Field label="Base, Hz" hint="Frequency of the offset channel">
            <NumInput label="Band plan base" placeholder="489087500" value={sys.bandplanBaseHz} onChange={(v) => setNum("bandplanBaseHz", v)} />
          </Field>
          <Field label="Spacing, Hz">
            <NumInput label="Band plan spacing" placeholder="25000" value={sys.bandplanSpacingHz} onChange={(v) => setNum("bandplanSpacingHz", v)} />
          </Field>
          <Field label="Offset (channel)">
            <NumInput label="Band plan offset" placeholder="380" value={sys.bandplanOffset} onChange={(v) => setNum("bandplanOffset", v)} />
          </Field>
          <Field label="High, Hz" hint="Highest outbound channel">
            <NumInput label="Band plan high" placeholder="496612500" value={sys.bandplanHighHz} onChange={(v) => setNum("bandplanHighHz", v)} />
          </Field>
        </div>
      )}
    </>
  );
}

function SystemCard(props: { c: Config; i: number }) {
  const { c, i } = props;
  const sys = c.systems[i];
  const [ccText, setCcText] = useState(() => sys.controlChannelsHz.map((f) => formatMhz(f)).join(", "));
  const need = useNeed();
  const plugins = useApp().plugins?.plugins ?? [];
  const edit = (fn: (x: System) => void) => updateConfig((x) => fn(x.systems[i]));
  const setExpect = (k: keyof SiteIdentity, v: number | null) =>
    edit((x) => {
      x.expect = { ...x.expect };
      if (v === null) delete x.expect[k];
      else x.expect[k] = v;
    });
  const color = systemColor(c, sys.shortName);
  const siblings = siteSiblings(c, sys);
  // The lock, on the fields this protocol states (others are ignored).
  const lockFields = siteLockFields(sys);
  const lockedTo: SiteIdentity = Object.fromEntries(lockFields.filter((f) => sys.expect[f] != null).map((f) => [f, sys.expect[f]]));
  const locked = Object.keys(lockedTo).length > 0;
  const dupName = c.systems.some((x, k) => k !== i && x.shortName === sys.shortName);
  // Where it lands on the sources.
  const centers = resolvedCenters(c);
  const ccOn = [...new Set(sys.controlChannelsHz.map((f) => sourceCovering(c, centers, f)).filter((k) => k >= 0))];
  const voiceIn = sys.voiceChannelsHz.filter((f) => sourceCovering(c, centers, f) >= 0).length;

  return (
    <div className={`system-card${sys.enabled ? "" : " off"}`} style={{ ["--sys-color" as string]: color }} id={`need-sys-${sys.shortName}`}>
      <div className="row sys-head">
        <span className="sys-dot" style={{ background: color }} />
        <strong>{sys.shortName || "(no name)"}</strong>
        {sys.enabled && sys.controlChannelsHz.length > 0 ? (
          ccOn.length ? (
            <span className="chip ok">control channel on source {ccOn.map((k) => k + 1).join(", ")}</span>
          ) : (
            <span className="chip bad">no control channel inside a source</span>
          )
        ) : null}
        {sys.enabled && sys.voiceChannelsHz.length > 0 && (
          <span className={`chip ${voiceIn === sys.voiceChannelsHz.length ? "ok" : "warn"}`} title="Voice channels found by the survey, inside a source">
            voice {voiceIn}/{sys.voiceChannelsHz.length} covered
          </span>
        )}
        {siblings.length > 0 && (
          <span
            className="chip"
            title={`${sys.siteGroup?.trim() ? `Site group "${sys.siteGroup.trim()}"` : siteText(sys.expect)} — ${c.recording.dropDuplicateCalls ? "calls saved once" : "every site's copy saved"}`}
          >
            multi-site with {siblings.map((x) => x.shortName).join(", ")}
          </span>
        )}
        <span className="spacer" />
        <label className="toggle small">
          <input type="checkbox" checked={sys.enabled} onChange={(e) => edit((x) => void (x.enabled = e.target.checked))} />
          <span>Record</span>
        </label>
        <button
          className="btn ghost small"
          onClick={() => {
            if (confirm(`Remove ${sys.shortName}? Its recordings stay on disk.`)) updateConfig((x) => void x.systems.splice(i, 1));
          }}
        >
          Remove
        </button>
      </div>
      <div className="grid2">
        <Field label="Short name" hint={dupName ? "Already used — each needs its own folder" : "Its calls' folder name"}>
          <input
            value={sys.shortName}
            onChange={(e) => {
              const to = e.target.value.replace(/[^\w.-]/g, "");
              // Plugin settings that name it follow it.
              updateConfig((x) => {
                renameSystemRefs(x, x.systems[i].shortName, to, plugins);
                x.systems[i].shortName = to;
              });
            }}
          />
        </Field>
        <Field label="Name">
          <input value={sys.name ?? ""} placeholder="County Public Safety" onChange={(e) => edit((x) => void (x.name = e.target.value))} />
        </Field>
        <Field label="Type">
          <select
            value={sys.type}
            onChange={(e) =>
              edit((x) => {
                x.type = e.target.value === "smartnet" ? "smartnet" : e.target.value === "dmr" ? "dmr" : e.target.value === "nxdn" ? "nxdn" : "p25";
                if (x.type === "smartnet" && !x.bandplan) x.bandplan = "800_standard";
              })
            }
          >
            <option value="p25">P25</option>
            <option value="smartnet">SmartNet / SmartZone</option>
            <option value="dmr">DMR (trunked)</option>
            <option value="nxdn">NXDN (trunked)</option>
          </select>
        </Field>
        {sys.type !== "dmr" && sys.type !== "nxdn" && <Field label={sys.type === "smartnet" ? "P25 voice modulation" : "Modulation"}>
          <select value={sys.modulation} onChange={(e) => edit((x) => void (x.modulation = e.target.value as System["modulation"]))}>
            <option value="auto">Auto (both receivers)</option>
            <option value="fsk4">C4FM (fsk4)</option>
            <option value="qpsk">CQPSK / LSM simulcast (qpsk)</option>
          </select>
        </Field>}
        <Field
          needs={need(`cc-${sys.shortName}`)}
          anchor={`cc-${sys.shortName}`}
          label={sys.type === "dmr" ? "Site frequencies, MHz" : sys.type === "nxdn" && sys.nxdnType === "typeD" ? "Repeaters, MHz" : "Control channels, MHz"}
          hint={
            sys.type === "dmr"
              ? "Capacity Plus: every repeater. Others: control channels."
              : sys.type === "nxdn" && sys.nxdnType === "typeD"
                ? "Every repeater of the site"
                : "Later ones are fallbacks"
          }
          wide
        >
          <input
            className="mono"
            value={ccText}
            placeholder="857.9875, 858.9875"
            onChange={(e) => {
              setCcText(e.target.value);
              const list = parseFreqList(e.target.value);
              edit((x) => void (x.controlChannelsHz = list));
            }}
          />
        </Field>
        <TalkgroupsField sys={sys} index={i} needs={need(`tg-${sys.shortName}`)} anchor={`tg-${sys.shortName}`} />
        <UnitNamesField
          value={sys.unitNames}
          anchor={`units-${sys.shortName}`}
          needs={need(`units-${sys.shortName}`)}
          onChange={(u) =>
            edit((x) => {
              if (u) x.unitNames = u;
              else delete x.unitNames;
            })
          }
        />
        {sys.type === "smartnet" && <SmartnetFields sys={sys} edit={edit} />}
        {sys.type === "dmr" && <DmrFields sys={sys} edit={edit} />}
        {sys.type === "nxdn" && <NxdnFields sys={sys} edit={edit} />}
        <Field
          label="Site group"
          hint={
            sys.type === "dmr" || (sys.type === "nxdn" && sys.nxdnType === "typeD")
              ? "Same name on each site: calls saved once"
              : "Only to join ISSI-linked systems or split a site"
          }
        >
          <input
            value={sys.siteGroup ?? ""}
            placeholder={sys.type === "dmr" || (sys.type === "nxdn" && sys.nxdnType === "typeD") ? "none" : "from the air"}
            onChange={(e) =>
              edit((x) => {
                if (e.target.value.trim()) x.siteGroup = e.target.value;
                else delete x.siteGroup;
              })
            }
          />
        </Field>
      </div>
      <SystemPluginSettings system={i} />
      <RecordingOverridePanel
        c={c}
        value={sys.recording}
        onChange={(fn) =>
          edit((x) => {
            x.recording = { ...x.recording };
            fn(x.recording);
            if (!Object.keys(x.recording).length) delete x.recording;
          })
        }
      />
      {lockFields.length > 0 && (
        <details className={`help${need(`site-${sys.shortName}`) ? " needs" : ""}`} id={`need-site-${sys.shortName}`} open={need(`site-${sys.shortName}`) ? true : undefined}>
          <summary>
            Site lock{locked ? <span className="muted"> — only {siteText(lockedTo)}</span> : <span className="muted"> — off</span>}
          </summary>
          <p className="muted small">
            {need(`site-${sys.shortName}`) && (
              <>
                <span className="field-needs">{need(`site-${sys.shortName}`)}</span>{" "}
              </>
            )}
            Follow only a control channel announcing this site. Empty = any; {sys.type === "smartnet" || sys.type === "nxdn" ? "System ID in hex" : "NAC, WACN and System ID in hex"}.
          </p>
          <div className="id-grid">
            {LOCK_INPUTS.filter((x) => lockFields.includes(x.key)).map((x) => (
              <Field key={x.key} label={x.label}>
                <NumInput label={x.label} hex={x.hex} value={sys.expect[x.key]} onChange={(v) => setExpect(x.key, v)} />
              </Field>
            ))}
          </div>
          {locked && (
            <button className="btn ghost small" onClick={() => edit((x) => void (x.expect = {}))}>
              Clear the lock
            </button>
          )}
        </details>
      )}
    </div>
  );
}

/**
 * Names for a system's radios: Trunk Recorder's unitTagsFile (loaded into the
 * config) and unitTagsMode — which comes first, these or the aliases radios send.
 */
function UnitNamesField(props: { value: UnitNames | undefined; onChange: (u: UnitNames | undefined) => void; anchor: string; needs?: string }) {
  const ref = useRef<HTMLInputElement>(null);
  const u = props.value ?? {};
  const set = (patch: Partial<UnitNames>) => {
    const n: UnitNames = { ...u, ...patch };
    for (const k of Object.keys(n) as (keyof UnitNames)[]) if (!n[k]) delete n[k];
    props.onChange(Object.keys(n).length ? n : undefined);
  };
  const count = u.csv ? unitNameCount(u.csv) : 0;
  return (
    <Field label="Unit names" needs={props.needs} anchor={props.anchor}>
      <div className="row">
        <button className="btn" onClick={() => ref.current?.click()}>
          Load CSV…
        </button>
        <span className="mono">{count ? `${count} from ${u.name ?? "file"}` : "none"}</span>
        {count > 0 && (
          <button className="btn ghost" onClick={() => set({ csv: undefined, name: undefined })}>
            Clear
          </button>
        )}
        <select value={u.mode ?? ""} aria-label="Which names come first" onChange={(e) => set({ mode: e.target.value as UnitNames["mode"] })}>
          <option value="">These first, then aliases heard</option>
          <option value="ota">Aliases heard first</option>
          <option value="user_only">Only these</option>
          <option value="none">No names</option>
        </select>
      </div>
      <input
        ref={ref}
        type="file"
        accept=".csv,text/csv,text/plain"
        hidden
        onChange={async (e) => {
          const f = e.target.files?.[0];
          e.target.value = "";
          if (!f) return;
          const csv = await f.text();
          set({ csv, name: f.name });
          setNotice(`Loaded ${unitNameCount(csv)} unit names from ${f.name}.`);
        }}
      />
    </Field>
  );
}

/** The log (desktop app): Trunk Recorder's options, applied at once. */
function LogPanel(props: { c: Config }) {
  const l: LogSettings = props.c.log ?? defaultLog();
  const set = (patch: Partial<LogSettings>) => updateConfig((x) => void (x.log = { ...l, ...patch }));
  return (
    <section className="panel">
      <header className="panel-head">
        <h2>Log</h2>
      </header>
      <div className="stack">
        <p className="muted small">
          <code>--log-level</code> on the command line overrides Level.
        </p>
        <div className="grid3">
          <Field label="Level" hint="Debug adds unrecorded calls; trace, every control message">
            <select value={l.level} onChange={(e) => set({ level: e.target.value as LogSettings["level"] })}>
              {["trace", "debug", "info", "warning", "error", "fatal"].map((v) => (
                <option key={v} value={v}>
                  {v}
                </option>
              ))}
            </select>
          </Field>
          <Field label="Frequencies as">
            <select value={l.frequencyFormat} onChange={(e) => set({ frequencyFormat: e.target.value as LogSettings["frequencyFormat"] })}>
              <option value="mhz">857.987500 MHz</option>
              <option value="hz">857987500 Hz</option>
              <option value="exp">8.579875e+08</option>
            </select>
          </Field>
          <Field label="Talkgroups as">
            <select value={l.talkgroupDisplayFormat} onChange={(e) => set({ talkgroupDisplayFormat: e.target.value as LogSettings["talkgroupDisplayFormat"] })}>
              <option value="id">3747</option>
              <option value="id_tag">3747 (DCFD Disp)</option>
              <option value="tag_id">(DCFD Disp) 3747</option>
            </select>
          </Field>
          <Field label="Colour">
            <select value={l.color} onChange={(e) => set({ color: e.target.value })}>
              <option value="">Console, on a terminal</option>
              <option value="console">Console</option>
              <option value="logfile">Log files</option>
              <option value="all">Both</option>
              <option value="none">None</option>
            </select>
          </Field>
          <Field label="Decode rate warning, msg/s" hint="Fewer is logged as an error; −1 always logs it">
            <DecInput label="Decode rate warning" value={l.controlWarnRatePerS} onChange={(v) => set({ controlWarnRatePerS: v ?? 10 })} />
          </Field>
        </div>
        <Toggle label="To the console" hint="stderr" checked={l.console} onChange={(v) => set({ console: v })} />
        <Toggle label="To files" hint="a new one daily and at 100 MB" checked={l.file} onChange={(v) => set({ file: v })} />
        {l.file && (
          <div className="grid2">
            <Field label="Folder" hint="Relative to the config file">
              <input className="mono" value={l.dir} placeholder="logs" onChange={(e) => set({ dir: e.target.value })} />
            </Field>
            <Toggle
              label="One file, for logrotate"
              hint="trunk-pro.log, never rotated here; SIGHUP reopens it"
              checked={l.syslogFriendly}
              onChange={(v) => set({ syslogFriendly: v })}
            />
          </div>
        )}
        <Toggle label="To the system log" checked={l.syslog} onChange={(v) => set({ syslog: v })} />
        <Toggle label="States as words" hint="“Monitoring”, not a number" checked={l.statusAsString} onChange={(v) => set({ statusAsString: v })} />
      </div>
    </section>
  );
}

type BoolRule = { [K in keyof RecordingRules]: RecordingRules[K] extends boolean ? K : never }[keyof RecordingRules];
type NumRule = { [K in keyof RecordingRules]: RecordingRules[K] extends number ? K : never }[keyof RecordingRules];

/**
 * The call rules every system follows unless it has its own (the recorder's
 * RecordingOverride), named and described after Trunk Recorder's settings
 * (in brackets). `desktop`: about files, which the browser keeps itself.
 */
type Rule =
  | { key: BoolRule; kind: "bool"; label: string; hint?: string; desktop?: boolean }
  | { key: NumRule; kind: "num"; label: string; hint?: string; min: number; max: number; zero?: string }
  | { key: "filenameFormat"; kind: "text"; label: string };

const RULE_GROUPS: { title: string; rules: Rule[] }[] = [
  {
    title: "What is recorded",
    rules: [
      { key: "recordUnknown", kind: "bool", label: "Talkgroups not in the talkgroup file" },
      { key: "recordUnitToUnit", kind: "bool", label: "Unit-to-unit calls", hint: "private calls between two radios" },
      { key: "callTimeoutS", kind: "num", label: "Call timeout, s", hint: "Silence that ends a call", min: 1, max: 30 },
    ],
  },
  {
    title: "Which calls are kept",
    rules: [
      { key: "minCallS", kind: "num", label: "Shortest call, s", hint: "Shorter calls are deleted, not uploaded", min: 0, max: 60, zero: "keep all" },
      {
        key: "minTransmissionS",
        kind: "num",
        label: "Shortest transmission, s",
        hint: "Shorter key-ups and bursts are dropped",
        min: 0,
        max: 10,
        zero: "keep all",
      },
      { key: "maxCallS", kind: "num", label: "Longest call, s", hint: "Longer calls are split, nothing lost", min: 0, max: 3600, zero: "no limit" },
      { key: "keepSilentCalls", kind: "bool", label: "Calls with no audio", hint: "encrypted or undecoded" },
    ],
  },
  {
    title: "Audio",
    rules: [
      { key: "normalizeAudio", kind: "bool", label: "Even out call loudness" },
      { key: "digitalLevelDb", kind: "num", label: "Digital Level Adjustment, dB", min: -20, max: 20 },
      { key: "analogLevelDb", kind: "num", label: "Analog Level Adjustment, dB", min: -20, max: 20 },
    ],
  },
  {
    title: "Files",
    rules: [
      { key: "filenameFormat", kind: "text", label: "Folders and file names" },
      { key: "compressWav", kind: "bool", label: "Also save an M4A", hint: "a tenth the size; needs ffmpeg or afconvert", desktop: true },
      { key: "audioArchive", kind: "bool", label: "Keep the audio after uploading", hint: "off: deleted once uploaded", desktop: true },
      { key: "callLog", kind: "bool", label: "Keep the call JSON after uploading", desktop: true },
      { key: "archiveFilesOnFailure", kind: "bool", label: "Keep everything when an upload fails", hint: "even with the two above off", desktop: true },
    ],
  },
];

const ruleShown = (r: Rule) => !(web && r.kind === "bool" && r.desktop);

function ruleValue(r: Rule, v: Recording[keyof Recording] | undefined): string {
  if (r.kind === "bool") return v ? "on" : "off";
  if (r.kind === "text") return (v as string) || "the usual layout";
  return v === 0 && r.zero ? r.zero : String(v);
}

/** The call rules for the whole recorder (the Recording tab). */
function CallRules(props: { c: Config }) {
  const r = props.c.recording;
  const set = <K extends keyof RecordingRules>(k: K, v: RecordingRules[K]) => updateConfig((x) => void ((x.recording as RecordingRules)[k] = v));
  return (
    <section className="panel">
      <header className="panel-head">
        <h2>Call rules</h2>
      </header>
      <div className="stack">
        <p className="muted small">Defaults for every system; each can override them.</p>
        {RULE_GROUPS.map((g) => (
          <div key={g.title} className="rule-group">
            <h3>{g.title}</h3>
            <div className="grid2">
              {g.rules.filter(ruleShown).map((rule) =>
                rule.kind === "bool" ? (
                  <Toggle key={rule.key} label={rule.label} hint={rule.hint} checked={!!r[rule.key]} onChange={(v) => set(rule.key, v)} />
                ) : rule.kind === "num" ? (
                  <Field key={rule.key} label={rule.label} hint={rule.hint}>
                    <DecInput
                      label={rule.label}
                      value={r[rule.key]}
                      placeholder={rule.zero ? `0 = ${rule.zero}` : undefined}
                      onChange={(v) => set(rule.key, Math.max(rule.min, Math.min(rule.max, v ?? 0)))}
                    />
                  </Field>
                ) : (
                  <FilenameEditor
                    key={rule.key}
                    label={rule.label}
                    value={r.filenameFormat}
                    fallback={DEFAULT_FORMAT}
                    fallbackName="Trunk Recorder's layout"
                    root={web ? undefined : props.c.recording.captureDir}
                    onChange={(v) => set("filenameFormat", v.trim() ? v : "")}
                  />
                ),
              )}
            </div>
          </div>
        ))}
      </div>
    </section>
  );
}

/**
 * A system's own call rules (or the conventional channels'): each setting
 * left at Default follows the Recording tab.
 */
function RecordingOverridePanel(props: { c: Config; value: RecordingOverride | undefined; onChange: (fn: (o: RecordingOverride) => void) => void }) {
  const { c, value, onChange } = props;
  const own = value ?? {};
  const base = c.recording;
  const set = (k: keyof RecordingOverride, v: RecordingOverride[keyof RecordingOverride] | undefined) =>
    onChange((o) => {
      if (v === undefined) delete o[k];
      else (o as Record<string, unknown>)[k] = v;
    });
  const rules = RULE_GROUPS.flatMap((g) => g.rules).filter(ruleShown);
  const mine = rules.filter((r) => own[r.key] !== undefined);
  return (
    <details className="help override">
      <summary>
        Recording override
        <span className="muted">{mine.length ? ` — ${mine.length} changed` : " — defaults"}</span>
      </summary>
      <div className="grid3">
        {rules.map((rule) => {
          const mineNow = own[rule.key] !== undefined;
          if (rule.kind === "text")
            return (
              <FilenameEditor
                key={rule.key}
                label={rule.label}
                value={own.filenameFormat ?? ""}
                fallback={base.filenameFormat || DEFAULT_FORMAT}
                fallbackName={base.filenameFormat ? "from Recording" : "Trunk Recorder's layout"}
                root={web ? undefined : base.captureDir}
                onChange={(v) => set("filenameFormat", v.trim() ? v : undefined)}
              />
            );
          return (
            <Field key={rule.key} label={rule.label} hint={mineNow ? `Default: ${ruleValue(rule, base[rule.key])}` : undefined}>
              {rule.kind === "bool" ? (
                <select
                  className={mineNow ? "overridden" : ""}
                  value={own[rule.key] === undefined ? "" : own[rule.key] ? "on" : "off"}
                  onChange={(e) => set(rule.key, e.target.value === "" ? undefined : e.target.value === "on")}
                >
                  <option value="">Default ({ruleValue(rule, base[rule.key])})</option>
                  <option value="on">On</option>
                  <option value="off">Off</option>
                </select>
              ) : (
                <DecInput
                  label={rule.label}
                  value={own[rule.key]}
                  placeholder={`Default (${ruleValue(rule, base[rule.key])})`}
                  onChange={(v) => set(rule.key, v === undefined ? undefined : Math.max(rule.min, Math.min(rule.max, v)))}
                />
              )}
            </Field>
          );
        })}
      </div>
      {mine.length > 0 && (
        <div className="row">
          <span className="spacer" />
          <button className="btn ghost small" onClick={() => onChange((o) => rules.forEach((r) => delete o[r.key]))}>
            Reset to Defaults
          </button>
        </div>
      )}
    </details>
  );
}

/** The tab showing now (for showTodo, outside React). */
let shownTab: SetupTab = "systems";
const currentTab = () => shownTab;

const TABS: { id: SetupTab; label: string; icon: () => React.ReactNode }[] = [
  { id: "systems", label: "Systems", icon: IconTower },
  { id: "conventional", label: "Conventional", icon: IconAntenna },
  { id: "radios", label: "Radios", icon: IconDongle },
  { id: "recording", label: "Recording", icon: IconFolder },
  { id: "plugins", label: "Plugins", icon: IconPuzzle },
];

/** The setup page's tabs: how many of each, and what an import left to do there. */
function SetupTabs(props: { c: Config; tab: SetupTab }) {
  const s = useApp();
  const todos = openTodos(s);
  const count: Record<SetupTab, number | null> = {
    systems: props.c.systems.length,
    conventional: props.c.conventional.reduce((n, v) => n + v.channels.length, 0),
    radios: props.c.sources.length,
    recording: null,
    plugins: s.plugins ? s.plugins.plugins.length : null,
  };
  return (
    <nav className="setup-tabs" role="tablist" aria-label="Setup">
      {TABS.filter((t) => t.id !== "plugins" || !web).map((t) => {
        const todo = todos.filter((x) => tabOf(x.target) === t.id).length;
        return (
          <button key={t.id} role="tab" aria-selected={props.tab === t.id} className={props.tab === t.id ? "on" : ""} onClick={() => setSetupTab(t.id)}>
            <t.icon />
            <span>{t.label}</span>
            {count[t.id] !== null && <span className="tab-count">{count[t.id]}</span>}
            {todo > 0 && (
              <span className="tab-todo" title={`${todo} to finish here`}>
                {todo}
              </span>
            )}
          </button>
        );
      })}
    </nav>
  );
}

export function Setup() {
  const s = useApp();
  const c = s.config;
  const tab = s.setupTab;
  shownTab = tab;
  // A source just added on the Radios tab (measuring its roll-off is offered).
  const [freshSource, setFreshSource] = useState<number | null>(null);
  if (!c) return <p className="muted">Connecting to the recorder…</p>;

  return (
    <div className="setup">
      <TodoPanel />
      <SetupTabs c={c} tab={tab} />
      {tab === "systems" && (
      <>
      <SurveyPanel c={c} />
      <section className="panel">
        <header className="panel-head">
          <h2>Systems</h2>
          <div className="row">
            <button className="btn ghost" onClick={() => updateConfig((x) => void x.systems.push(newSystem(x)))}>
              Add a system
            </button>
            <button className="btn ghost" onClick={() => openGuide("import")}>
              Import Trunk Recorder config…
            </button>
          </div>
        </header>
        <div className="stack">
          {c.systems.map((_, i) => (
            <SystemCard key={`${i}-${c.systems.length}-${s.configEpoch}`} c={c} i={i} />
          ))}
          {c.systems.length === 0 ? (
            <p className="empty small">No trunked systems yet.</p>
          ) : null}
        </div>
      </section>
      </>
      )}

      {tab === "conventional" && <ConventionalTab c={c} />}

      {tab === "radios" && (
      <section className="panel">
        <header className="panel-head">
          <h2>Radios</h2>
          <button
            className="btn ghost"
            onClick={() => {
              setFreshSource(c.sources.length);
              updateConfig((x) => void x.sources.push(newDongle()));
            }}
          >
            Add a source
          </button>
        </header>
        <div className="stack">
          {c.sources.map((_, i) => (
            <SourceCard key={`${i}-${s.configEpoch}`} c={c} i={i} fresh={freshSource === i} onFreshDone={() => setFreshSource(null)} />
          ))}
          <details className="help">
            <summary>Dongle not showing up?</summary>
            {web ? (
              <ul>
                <li>Needs Chrome or Edge (WebUSB).</li>
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
                <b>Windows:</b> install the WinUSB driver for “Bulk-In, Interface 0” with Zadig.
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
      )}

      {tab === "recording" && (
      <section className="panel">
        <header className="panel-head">
          <h2>Recorder</h2>
        </header>
        <div className="stack">
          {web ? (
            <p className="muted small">Calls are kept in this browser; <i>Recorded calls</i> exports them.</p>
          ) : (
            <Field label="Recordings folder" wide>
              <input className="mono" value={c.recording.captureDir} onChange={(e) => updateConfig((x) => void (x.recording.captureDir = e.target.value))} />
            </Field>
          )}
          {!web && (
            <Toggle
              label="Keep calls waiting to upload in memory"
              hint="a RAM disk, not the recordings folder; from the next start"
              checked={c.recording.ramSpool?.enabled ?? false}
              onChange={(v) => updateConfig((x) => void (x.recording.ramSpool = { sizeMb: 256, ...x.recording.ramSpool, enabled: v }))}
            />
          )}
          {!web && c.recording.ramSpool?.enabled && (
            <Field label="RAM spool size, MB" hint="When full, calls go to the recordings folder">
              <input
                className="mono"
                value={c.recording.ramSpool.sizeMb}
                onChange={(e) => updateConfig((x) => void (x.recording.ramSpool = { ...x.recording.ramSpool!, sizeMb: Math.max(16, Math.min(8192, Number(e.target.value) || 256)) }))}
              />
            </Field>
          )}
          <div className="grid3">
            <Field label="Recorders" hint="Calls recorded at once">
              <input className="mono" value={c.recording.maxRecorders} onChange={(e) => updateConfig((x) => void (x.recording.maxRecorders = Math.max(1, Math.min(64, Number(e.target.value) || 32))))} />
            </Field>
            <Field label="Pre-roll, s" hint="Audio kept from before each grant">
              <input className="mono" value={c.recording.prerollS} onChange={(e) => updateConfig((x) => void (x.recording.prerollS = Math.max(0, Math.min(3, Number(e.target.value) || 0))))} />
            </Field>
            <Field label="P25 voice decoder" hint="Fixed-point usually sounds most natural">
              <select value={c.recording.vocoder ?? "fixed"} onChange={(e) => updateConfig((x) => void (x.recording.vocoder = e.target.value as "fixed" | "enhanced" | "mbelib"))}>
                <option value="fixed">Fixed-point</option>
                <option value="enhanced">Enhanced</option>
                <option value="mbelib">mbelib</option>
              </select>
            </Field>
          </div>
          <Toggle
            label="Save a call heard on several sites once"
            hint="keeps the cleanest copy, or the Preferred Site's"
            checked={c.recording.dropDuplicateCalls ?? true}
            onChange={(v) => updateConfig((x) => void (x.recording.dropDuplicateCalls = v))}
          />
          {!web && (
            <Toggle
              label="Start recording when the app starts"
              checked={c.server.autoStart}
              onChange={(v) => updateConfig((x) => void (x.server.autoStart = v))}
            />
          )}
          {!web && (
            <Toggle
              label="Save vocoder frames"
              hint="diagnostics, as <call>.frames.jsonl"
              checked={c.recording.captureFrames}
              onChange={(v) => updateConfig((x) => void (x.recording.captureFrames = v))}
            />
          )}
        </div>
      </section>
      )}
      {tab === "recording" && <CallRules c={c} />}
      {tab === "recording" && !web && <LogPanel c={c} />}
      {tab === "recording" && !web && <M4aSettings />}
      {tab === "recording" && !web && <InterfacesPanel />}

      {tab === "plugins" && !web && <PluginSetupPanel />}
    </div>
  );
}
