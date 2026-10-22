import { Fragment, useId, useRef, useState } from "react";
import {
  activeSites,
  activeSystems,
  AIRSPY_RATES,
  AM_LOW_CUT_MAX_HZ,
  BANDWIDTH_PRESETS,
  defaultBandwidthKhz,
  SQUELCH_HANG_MAX_MS,
  SQUELCH_OPEN_MAX_MS,
  BLADERF_RATES,
  channelsToCsv,
  channelTalkgroups,
  defaultLog,
  enabledChannels,
  formatFromPath,
  formatMhz,
  mhzCell,
  newAirspy,
  newBladerf,
  newConventional,
  newDongle,
  newFile,
  newSoapy,
  newUsrp,
  nextTalkgroup,
  parseChannelCsv,
  parseFreqList,
  newSiteName,
  newSystem,
  resolvedCenters,
  sameOnTheAir,
  siteActive,
  siteNameOf,
  SAMPLE_RATES,
  SOAPY_RATES,
  sourceCovering,
  systemColor,
  USRP_RATES,
  usableHalfWidth,
  siteLockFields,
  siteText,
  systemType,
} from "./config.ts";
import {
  can,
  cardKey,
  currentView,
  dismissTodo,
  downloadText,
  findRadios,
  forgetTalkgroups,
  moveTalkgroups,
  renameSystem,
  newSiteOf,
  openGuide,
  refreshDevices,
  setChannelFile,
  setNotice,
  setSetupTab,
  setView,
  unlockKeys,
  updateConfig,
  useApp,
  web,
  type SetupTab,
} from "./controller.ts";
import { M4aSettings, PluginSetupPanel, SystemPluginSettings, useM4aPlugins } from "./Plugins.tsx";
import { renameSystemRefs } from "./pluginSettings.ts";
import { SystemDelays } from "./Delays.tsx";
import { IconAntenna, IconDongle, IconFolder, IconPuzzle, IconTower } from "./Onboarding.tsx";
import type { AirspyGainMode, Channel, Config, Conventional, Demod, Device, HeardCode, LockableKeys, LogSettings, Recording, RecordingOverride, RecordingRules, Site, SiteIdentity, SoapyState, Source, System, UnitNames } from "./protocol.ts";
import { DEFAULT_FORMAT, FilenameEditor } from "./FilenameEditor.tsx";
import { GuardBand } from "./Rolloff.tsx";
import { SurveyPanel } from "./Survey.tsx";
import { FccTable, IdentifyFcc } from "./Identify.tsx";
import { siteFrequencies } from "./controller.ts";
import { addCard, Card, ExpandAll, Group, Section, setCardOpen, Switch } from "./SetupLayout.tsx";
import { TalkgroupsField } from "./TalkgroupsField.tsx";
import { DraftInput, Field, FileInput, NumberInput, Toggle } from "./inputs.tsx";
import { asTyped, cleanShortName, lcnTable, mhz, mhzList, msps } from "./fieldText.ts";
import { unitNameCount } from "./units.ts";
import { openTodos } from "./todo.ts";
import { parseAccess, sameTone } from "./tones.ts";

/** Each card's key (the controller's `cardKey`), seeded from what its entry is now; `all`: its list. */
const srcKey = (x: Source, i: number, all: Source[]) => cardKey(x, `src:${i}`, all);
const sysKey = (x: System, _i: number, all: System[]) => cardKey(x, `sys:${x.shortName}`, all);
const siteKey = (sys: System, x: Site) => cardKey(x, `site:${sys.shortName}/${siteNameOf(sys, x)}`, sys.sites);
const convKey = (x: Conventional, _i: number, all: Conventional[]) => cardKey(x, `conv:${x.shortName}`, all);

/** What an import left to do at a setup field (see todo.ts), or undefined. */
function useNeed(): (target: string) => string | undefined {
  const todos = openTodos(useApp());
  return (target) => todos.find((t) => t.target === target)?.text;
}

/** How many of an import's to-dos are at these fields (`<kind>-<name>`). */
function useTodosAt(kinds: string[], name: string): number {
  return openTodos(useApp()).filter((t) => kinds.some((k) => t.target === `${k}-${name}`)).length;
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
  // In a folded card (a site's, in its system's): open them, then look again.
  const folded: HTMLElement[] = [];
  for (let card = el?.closest<HTMLElement>("[data-card]"); card; card = card.parentElement?.closest<HTMLElement>("[data-card]")) if (card.dataset.collapsed) folded.push(card);
  if (folded.length) {
    for (const card of folded) setCardOpen(card.dataset.card!, true);
    return void setTimeout(() => showTodo(target, tries), 60);
  }
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

/** A key container waiting for its password, or unlocked. */
function KeyContainer(props: { k: LockableKeys }) {
  const { k } = props;
  const [password, setPassword] = useState("");
  const state = <span className={`chip ${k.unlocked ? "ok" : "warn"}`}>{k.unlocked ? "Unlocked" : "Locked"}</span>;
  return (
    <section className="set-card">
      <Group title={k.name} hint={`For ${k.systems.join(", ")}`} actions={state} cols={1}>
        {k.unlocked ? (
          <div className="row">
            <span className="muted small">Its systems decrypt with these keys until you lock them or the app quits.</span>
            <button className="btn" onClick={() => unlockKeys(k.name, null)}>
              Lock
            </button>
          </div>
        ) : (
          <form
            className="row"
            onSubmit={(e) => {
              e.preventDefault();
              if (!password) return;
              unlockKeys(k.name, password);
              setPassword("");
            }}
          >
            <Field label="Password">
              <input type="password" autoComplete="off" value={password} onChange={(e) => setPassword(e.target.value)} />
            </Field>
            <button className="btn primary" type="submit" disabled={!password}>
              Unlock
            </button>
          </form>
        )}
      </Group>
    </section>
  );
}

/** A conventional channel's modes, as the mode menus list them. */
const MODE_OPTIONS: { mode: Channel["mode"]; label: string }[] = [
  { mode: "fm", label: "Analog FM" },
  { mode: "am", label: "Analog AM" },
  { mode: "p25", label: "P25" },
  { mode: "dmr", label: "DMR" },
  { mode: "nxdn48", label: "NXDN48 (6.25 kHz)" },
  { mode: "nxdn96", label: "NXDN96 (12.5 kHz)" },
];

const modeOptions = MODE_OPTIONS.map((o) => (
  <option key={o.mode} value={o.mode}>
    {o.label}
  </option>
));

const TONE_HINT = {
  fm: { placeholder: "any", title: "CTCSS tone or DCS code (151.4 PL, 023 DPL). Empty: any." },
  am: { placeholder: "—", title: "AM carries no tone or code." },
  p25: { placeholder: "any NAC", title: "NAC, hex (293). Empty: any." },
  dmr: { placeholder: "any CC", title: "Colour code, optional slot and talkgroup (CC1 TS2 TG201). Empty: any." },
  nxdn48: { placeholder: "any RAN", title: "RAN, optional talkgroup (RAN 5 TG 201). Empty or RAN 0: any." },
  nxdn96: { placeholder: "any RAN", title: "RAN, optional talkgroup (RAN 5 TG 201). Empty or RAN 0: any." },
};

/** A channel's code (CTCSS / DCS, NAC, colour code, RAN): kept as typed, tidied when left. */
function ToneInput(props: { mode: Channel["mode"]; tone: string; disabled?: boolean; onChange: (tone: string) => void }) {
  const parsed = parseAccess(props.mode, props.tone);
  const error = "error" in parsed ? parsed.error : null;
  return (
    <DraftInput
      className={`mono narrow${error ? " invalid" : ""}`}
      disabled={props.disabled}
      value={props.tone}
      codec={asTyped}
      placeholder={TONE_HINT[props.mode].placeholder}
      readOnly={props.mode === "am" && !props.tone}
      aria-label="Tone"
      aria-invalid={!!error}
      title={error ?? TONE_HINT[props.mode].title}
      onChange={props.onChange}
      onBlur={() => "tone" in parsed && parsed.tone !== props.tone && props.onChange(parsed.tone)}
    />
  );
}

const NONE_LABEL = { fm: "no tone", am: "no code", p25: "no NAC", dmr: "no code", nxdn48: "no code", nxdn96: "no code" };
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
      <td colSpan={8}>
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
      <DraftInput className="mono" list={id} codec={msps} value={props.hz} onChange={(hz) => hz > 0 && props.onChange(hz)} />
      <datalist id={id}>
        {props.options.map((r) => (
          <option key={r} value={r / 1e6} />
        ))}
      </datalist>
    </>
  );
}

/** Why an optional driver can't be used, and how to install it. */
function DriverMissing(props: { kind: "usrp" | "airspy" | "bladerf" | "soapy"; detail: string }) {
  return (
    <div className="banner bad small wide">
      {props.kind === "soapy" ? (
        <span>
          SoapySDR support needs <b>SoapySDR</b> and your radio&apos;s module, which weren&apos;t found ({props.detail}). Install them — macOS:{" "}
          <code>brew install soapysdr soapyhackrf</code>; Debian/Ubuntu: <code>sudo apt install soapysdr0.8-module-all</code>; Windows: PothosSDR or
          radioconda — then restart Trunk Recorder Pro.
        </span>
      ) : props.kind === "bladerf" ? (
        <span>
          bladeRF support needs <b>libbladeRF 2.x</b>, which wasn&apos;t found ({props.detail}). Install it — Debian/Ubuntu:{" "}
          <code>sudo apt install libbladerf2 bladerf-fpga-hostedxa4</code> (or Nuand&apos;s PPA, <code>ppa:nuandllc/bladerf</code>); macOS:{" "}
          <code>brew install libbladerf</code>; Windows: Nuand&apos;s bladeRF installer — then restart Trunk Recorder Pro.
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

function PpmField(props: { src: Exclude<Source, { type: "file" }>; edit: EditSource }) {
  const { src, edit } = props;
  return (
    <Field label="Frequency correction, ppm" hint={src.type === "rtlsdr" ? "Whole numbers" : undefined}>
      <NumberInput
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

/** USB transfer sizes offered (Trunk Recorder's buflen), bytes; 0 = the driver's 256 KiB. */
const BUF_LENS = [0, 131_072, 65_536, 32_768, 16_384];
/** As many RTL-SDRs as this at 2 MS/s or more get the advice (as `Config::rtl_buflen_hint`). */
const RTL_BUSY_COUNT = 4;

/**
 * An RTL-SDR's USB transfer size: smaller transfers let several dongles take
 * turns on one USB 2.0 bus without any of their small FIFOs overflowing.
 */
function UsbTransfers(props: { src: Extract<Source, { type: "rtlsdr" }>; sources: Source[]; devices: Device[]; edit: EditSource }) {
  const { src, sources, devices, edit } = props;
  const len = src.bufLen ?? 0;
  const busy = sources.filter((x) => x.type === "rtlsdr" && x.rateHz >= 2_000_000).length;
  const mine = devices.find((d) => (src.serial ? d.serial === src.serial : true));
  const onBus = mine?.bus ? devices.filter((d) => d.bus === mine.bus).length : null;
  const crowded = busy >= RTL_BUSY_COUNT && (onBus === null || onBus >= RTL_BUSY_COUNT);
  const hint =
    crowded && len === 0
      ? `${busy} dongles at full rate${onBus !== null ? `, ${onBus} on this USB bus` : ""}: 64 KiB recommended`
      : "Smaller helps several dongles share one USB 2.0 bus (Trunk Recorder's buflen)";
  const kib = (n: number) => `${n / 1024} KiB`;
  return (
    <Field label="USB transfer size" hint={hint}>
      <select value={len} onChange={(e) => edit((x) => x.type === "rtlsdr" && void (x.bufLen = Number(e.target.value)))}>
        {BUF_LENS.map((n) => (
          <option key={n} value={n}>
            {n === 0 ? "256 KiB (default)" : kib(n)}
          </option>
        ))}
        {!BUF_LENS.includes(len) && <option value={len}>{len % 1024 ? `${len} bytes` : kib(len)}</option>}
      </select>
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
        hint="follows crystal drift using its control channels and digital calls"
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
            <NumberInput narrow label={`${k} gain, dB`} value={v} disabled={disabled} onChange={(n) => set((g) => void (g[k] = n ?? 0))} />
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

const KINDS: { kind: Source["type"]; label: string; drivers?: boolean }[] = [
  { kind: "rtlsdr", label: "RTL-SDR" },
  { kind: "usrp", label: "USRP", drivers: true },
  { kind: "airspy", label: "Airspy", drivers: true },
  { kind: "bladerf", label: "bladeRF", drivers: true },
  { kind: "soapy", label: "SoapySDR", drivers: true },
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
      const fresh = kind === "rtlsdr" ? newDongle() : kind === "usrp" ? newUsrp() : kind === "airspy" ? newAirspy() : kind === "bladerf" ? newBladerf() : kind === "soapy" ? newSoapy() : newFile();
      x.sources[i] = { ...fresh, centerHz: old.centerHz } as Source;
    });
  const fileRef = useRef<HTMLInputElement>(null);
  const others = c.sources.filter((_, k) => k !== i);
  const used = new Set(others.map((x) => (x.type === "rtlsdr" ? `r:${x.serial}` : x.type === "airspy" ? `a:${x.serial}` : x.type === "bladerf" ? `b:${x.serial}` : x.type === "usrp" ? `u:${x.args}` : x.type === "soapy" ? `s:${x.args}` : "")));
  const radios = s.radios;
  const needs = useNeed()(`src-${i}`);

  const kindLabel = KINDS.find((k) => k.kind === src.type)?.label ?? src.type;
  const tuned = src.centerHz || auto;

  return (
    <Card
      cardKey={srcKey(src, i, c.sources)}
      n={c.sources.length}
      className={`source-card${needs ? " needs" : ""}`}
      id={`need-src-${i}`}
      head={
        <>
          <div className="set-card-title">
            <h3>Source {i + 1}</h3>
            <span className="set-card-sub">
              {kindLabel}
              {tuned ? ` · ${formatMhz(tuned, 3)} MHz` : ""} · {(src.rateHz / 1e6).toFixed(src.rateHz % 1e5 ? 3 : 1)} MSPS
            </span>
          </div>
          {needs && <span className="chip warn">to finish</span>}
        </>
      }
      actions={
        c.sources.length > 1 && (
          <button
            className="btn ghost small"
            onClick={() => {
              web?.removeFile(i);
              updateConfig((x) => void x.sources.splice(i, 1));
            }}
          >
            Remove
          </button>
        )
      }
      note={needs && <div className="field-needs set-card-needs">{needs}</div>}
    >
      <Group title="Device" hint={src.type === "file" ? "A capture to replay instead of a radio." : "Which radio, and how it is connected."}>
        <div className="field wide">
          <span className="field-label">Kind</span>
          <div className="seg small" role="radiogroup" aria-label={`Source ${i + 1} kind`}>
            {KINDS.filter((k) => !k.drivers || can("drivers")).map((k) => (
              <button key={k.kind} role="radio" aria-checked={src.type === k.kind} className={src.type === k.kind ? "on" : ""} onClick={() => setKind(k.kind)}>
                {k.label}
              </button>
            ))}
          </div>
        </div>
        {src.type === "rtlsdr" && (
          <Field label="Dongle" hint={s.devices.length ? undefined : web ? "No dongle connected" : "No dongle found"}>
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
        {src.type === "rtlsdr" && can("drivers") && <UsbTransfers src={src} sources={c.sources} devices={s.devices} edit={edit} />}
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
                  className="mono grow"
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
        {src.type === "usrp" && (
          <Field label="Antenna" hint="e.g. RX2, TX/RX">
            <input className="mono" value={src.antenna} placeholder="default" onChange={(e) => edit((x) => x.type === "usrp" && void (x.antenna = e.target.value.trim()))} />
          </Field>
        )}
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
        {src.type === "airspy" && (
          <>
            <Toggle label="Bias-T" hint="Powers an LNA via the antenna cable" checked={src.biasTee} onChange={(v) => edit((x) => x.type === "airspy" && void (x.biasTee = v))} />
            <Toggle
              label="Pack samples"
              hint="12-bit samples on the USB: a quarter less of the bus (an Airspy at 10 MSPS nearly fills a USB 2.0 bus). Off only for old firmware"
              checked={src.packing ?? true}
              onChange={(v) => edit((x) => x.type === "airspy" && void (x.packing = v))}
            />
          </>
        )}
        {src.type === "bladerf" &&
          (radios?.bladerf && !radios.bladerf.available ? (
            <DriverMissing kind="bladerf" detail={radios.bladerf.detail} />
          ) : (
            <Field label="bladeRF" hint={radios?.bladerf?.devices?.length ? radios.bladerf.detail : `${radios?.bladerf?.detail ?? "libbladeRF"} · none found — plug one in and press Refresh`}>
              <div className="row">
                <select value={src.serial} onChange={(e) => edit((x) => x.type === "bladerf" && void (x.serial = e.target.value))}>
                  <option value="">First available</option>
                  {src.serial && !(radios?.bladerf?.devices ?? []).some((d) => d.serial === src.serial) && <option value={src.serial}>SN {src.serial} (not connected)</option>}
                  {(radios?.bladerf?.devices ?? []).map((d) => (
                    <option key={d.serial} value={d.serial} disabled={used.has(`b:${d.serial}`)}>
                      {d.label}
                      {used.has(`b:${d.serial}`) ? " (in use)" : ""}
                    </option>
                  ))}
                </select>
                <button className="btn ghost small" disabled={s.findingRadios} onClick={findRadios}>
                  Refresh
                </button>
              </div>
            </Field>
          ))}
        {src.type === "bladerf" && (
          <Toggle label="Bias-T" hint="Powers an LNA via the antenna cable (bladeRF 2.0)" checked={src.biasTee} onChange={(v) => edit((x) => x.type === "bladerf" && void (x.biasTee = v))} />
        )}
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
                    className="mono grow"
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
              <Field label="Antenna">
                <input className="mono" value={src.antenna} placeholder="default" onChange={(e) => edit((x) => x.type === "soapy" && void (x.antenna = e.target.value.trim()))} />
              </Field>
              <Field label="Device settings" hint="key=value, e.g. biastee=true (see SoapySDRUtil --probe)" wide>
                <input className="mono" value={src.settings} placeholder="none" onChange={(e) => edit((x) => x.type === "soapy" && void (x.settings = e.target.value))} />
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
              <FileInput
                inputRef={fileRef}
                onFile={(f) => {
                  web?.setFile(i, f ?? null);
                  edit((x) => x.type === "file" && void (x.path = f?.name ?? ""));
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
        {src.type === "file" && can("service") && (
          <Field label="Sample format">
            <select value={src.format ?? "cu8"} onChange={(e) => edit((x) => x.type === "file" && void (x.format = e.target.value as "cu8" | "cs16" | "cf32"))}>
              <option value="cu8">cu8 — rtl_sdr (unsigned 8-bit)</option>
              <option value="cs16">cs16 — signed 16-bit</option>
              <option value="cf32">cf32 — float (GNU Radio, UHD)</option>
            </select>
          </Field>
        )}
        {src.type === "file" && <Toggle label="Real-time pace" hint="Off: as fast as it decodes" checked={src.realtime} onChange={(v) => edit((x) => x.type === "file" && void (x.realtime = v))} />}
      </Group>
      <Group title="Tuning" hint="The slice of spectrum it hears. Systems and channels inside the shaded band are recorded.">
        <Field
          label="Center frequency, MHz"
          hint={src.centerHz ? undefined : auto ? `Auto: ${formatMhz(auto, 4)} MHz` : "Auto: no uncovered system to place over"}
        >
          <DraftInput className="mono" codec={mhz} value={src.centerHz} placeholder={auto ? formatMhz(auto, 4) : "MHz"} onChange={(hz) => edit((x) => void (x.centerHz = hz))} />
        </Field>
        <Field
          label="Sample rate"
          hint={src.type === "usrp" ? "MSPS; wider costs more CPU" : src.type === "airspy" ? "R2: 10 or 2.5; Mini: 6 or 3" : src.type === "bladerf" ? "MSPS, 0.52–61.44" : src.type === "soapy" ? "MSPS, one the device supports" : undefined}
        >
          <RateInput
            key={src.type}
            hz={src.rateHz}
            free={src.type === "usrp" || src.type === "bladerf" || src.type === "soapy" || (src.type === "file" && can("service"))}
            options={src.type === "usrp" ? USRP_RATES : src.type === "airspy" ? AIRSPY_RATES : src.type === "bladerf" ? BLADERF_RATES : src.type === "soapy" ? SOAPY_RATES : src.type === "file" ? [...SAMPLE_RATES, 8_000_000, 10_000_000] : SAMPLE_RATES}
            onChange={(hz) => edit((x) => void (x.rateHz = hz))}
          />
        </Field>
        {src.type === "bladerf" && (
          <Field label="Filter bandwidth, MHz" hint="The analog low-pass filter. Blank = the sample rate.">
            <DraftInput className="mono" codec={mhz} value={src.bandwidthHz} placeholder={formatMhz(src.rateHz, 2)} aria-label="Filter bandwidth, MHz" onChange={(hz) => edit((x) => x.type === "bladerf" && void (x.bandwidthHz = hz))} />
          </Field>
        )}
        <GuardBand c={c} i={i} fresh={props.fresh} onFreshDone={props.onFreshDone} />
        {center ? (
          <div className="wide">
            <CoverageBar c={c} center={center} rateHz={src.rateHz} guardHz={src.guardHz} />
          </div>
        ) : null}
      </Group>
      {src.type !== "file" && (
        <Group title="Gain" hint={src.type === "airspy" ? "A preset, or each stage set by hand." : "Too little loses weak signals; too much overloads near strong ones."}>
          {src.type === "rtlsdr" && (
            <Field label="Gain, dB" hint={src.agc ? "A fixed gain usually works better" : "Most dongles: 0–49.6"}>
              <div className="row">
                <NumberInput gain label="Gain, dB" value={src.gainDb} disabled={src.agc} onChange={(v) => v !== undefined && edit((x) => x.type === "rtlsdr" && void (x.gainDb = v))} />
                <Toggle label="AGC" checked={src.agc} onChange={(v) => edit((x) => x.type === "rtlsdr" && void (x.agc = v))} />
              </div>
            </Field>
          )}
          {src.type === "usrp" && (
            <Field label="Gain, dB" hint={src.agc ? "AGC: B200 / B210 and E3xx only" : "B200/B210: 0–76"}>
              <div className="row">
                <NumberInput gain label="Gain, dB" value={src.gainDb} disabled={src.agc} onChange={(v) => edit((x) => x.type === "usrp" && void (x.gainDb = v || 0))} />
                <Toggle label="AGC" checked={src.agc} onChange={(v) => edit((x) => x.type === "usrp" && void (x.agc = v))} />
              </div>
            </Field>
          )}
          {src.type === "bladerf" && (
            <Field label="Gain, dB" hint={src.agc ? "The board's AGC sets it. A fixed gain is usually better for trunking." : "Overall RX gain; bladeRF 2.0: about −15 to 60"}>
              <div className="row">
                <NumberInput gain label="Gain, dB" value={src.gainDb} disabled={src.agc} onChange={(v) => edit((x) => x.type === "bladerf" && void (x.gainDb = v ?? 0))} />
                <Toggle label="AGC" checked={src.agc} onChange={(v) => edit((x) => x.type === "bladerf" && void (x.agc = v))} />
              </div>
            </Field>
          )}
          {src.type === "airspy" && <AirspyGainFields src={src} edit={edit} />}
          {src.type === "soapy" && (
            <>
              <Field label="Gain, dB">
                <div className="row">
                  <NumberInput gain label="Gain, dB" value={src.gainDb ?? undefined} placeholder={src.agc ? "AGC" : "as is"} disabled={src.agc} onChange={(v) => edit((x) => x.type === "soapy" && void (x.gainDb = v ?? null))} />
                  <Toggle label="AGC" checked={src.agc} onChange={(v) => edit((x) => x.type === "soapy" && void (x.agc = v))} />
                </div>
              </Field>
              <GainStages i={i} gains={src.gains} disabled={src.agc} edit={edit} />
            </>
          )}
        </Group>
      )}
      <Group title="Frequency correction" hint="Its crystal's error, so channels land where they should.">
        {src.type !== "file" && <PpmField src={src} edit={edit} />}
        <AutoTune src={src} i={i} edit={edit} />
      </Group>
    </Card>
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
  activeSites(c).forEach(({ sys: x, site }) => {
    const name = siteNameOf(x, site);
    for (const f of site.controlChannelsHz) if (inside(f)) ticks.push({ hz: f, kind: "cc", color: systemColor(c, x.shortName), label: `${name} control channel ${formatMhz(f)} MHz` });
    for (const f of site.voiceChannelsHz) if (inside(f)) ticks.push({ hz: f, kind: "voice", color: systemColor(c, x.shortName), label: `${name} voice ${formatMhz(f)} MHz` });
  });
  const conv = enabledChannels(c).filter((ch) => inside(ch.freqHz));
  for (const ch of conv) ticks.push({ hz: ch.freqHz, kind: "voice", color: "var(--text)", label: `conventional ${ch.name || formatMhz(ch.freqHz)}` });
  const here = systems.filter((x) => x.sites.filter(siteActive).some((y) => y.controlChannelsHz.some(inside) || y.voiceChannelsHz.some(inside)));
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
    <Section
      title="Conventional systems"
      hint="Single frequencies (analog FM or AM, P25, DMR or NXDN), kept together as a system with its own folder."
      actions={
        <>
          <ExpandAll keys={c.conventional.map(convKey)} />
          <button
            className="btn"
            onClick={() => {
              updateConfig((x) => {
                const n = newConventional(x);
                x.conventional.push(n);
                addCard(c.conventional.map(convKey), convKey(n, x.conventional.length - 1, x.conventional));
              });
            }}
          >
            Add a conventional system
          </button>
        </>
      }
    >
      {c.conventional.map((x, k) => (
        <ConventionalPanel key={convKey(x, k, c.conventional)} c={c} k={k} />
      ))}
      {!c.conventional.length && <p className="set-empty">No conventional systems yet.</p>}
    </Section>
  );
}

/**
 * One conventional system: a table editor, bulk add, CSV import / export,
 * and (desktop) a linked CSV file to edit in a spreadsheet instead.
 */
function ConventionalPanel(props: { c: Config; k: number }) {
  const { c, k } = props;
  const need = useNeed();
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
  const linked = can("service") && !!conv.channelFile;
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
    if (ch.bandwidthKhz !== undefined) row.bandwidthKhz = ch.bandwidthKhz;
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
  const todo = useTodosAt(["channels", "squelch", "convunits", "convplug"], name);

  return (
    <Card
      cardKey={convKey(conv, k, c.conventional)}
      n={c.conventional.length}
      className={conv.enabled ? undefined : "off"}
      id={`need-conv-${name}`}
      head={
        <>
          <div className="set-card-title">
            <h3>{conv.name?.trim() || name || "(no name)"}</h3>
            <span className="set-card-sub">
              {conv.name?.trim() && name ? `${name} · ` : ""}
              {chans.length} channel{chans.length === 1 ? "" : "s"}
              {chans.length ? `, ${enabled} on` : ""}
            </span>
          </div>
          {todo > 0 && <span className="chip warn">{todo} to finish</span>}
        </>
      }
      actions={
        <>
          <Switch label="Record" checked={conv.enabled} onChange={(v) => edit((x) => void (x.enabled = v))} />
          <IdentifyFcc target={{ kind: "conventional", index: k }} what={conv.name || conv.shortName} freqs={[...new Set(chans.map((ch) => ch.freqHz).filter((f) => f > 0))]} fcc={conv.fcc} />
          <button
            className="btn ghost small"
            onClick={() => {
              if (window.confirm(`Remove ${name || "this conventional system"} and its ${chans.length} channel${chans.length === 1 ? "" : "s"}? Its recordings stay on disk.`)) {
                updateConfig((x) => {
                  x.conventional.splice(k, 1);
                  forgetTalkgroups(x, conv.shortName);
                });
              }
            }}
          >
            Remove
          </button>
          <FileInput inputRef={csvRef} accept=".csv,text/csv" onFile={(f) => void onCsv(f)} />
        </>
      }
    >
      <Group title="Basics" hint="The short name is its calls' folder." cols={3}>
        <Field label="Short name" hint={dupName ? "Already used — each needs its own folder" : undefined}>
          <input
            value={conv.shortName}
            onChange={(e) => {
              const to = cleanShortName(e.target.value);
              updateConfig((x) => renameSystem(x, x.conventional[k], to));
            }}
          />
        </Field>
        <Field label="Name">
          <input value={conv.name ?? ""} placeholder="County Fire" onChange={(e) => edit((x) => void (e.target.value ? (x.name = e.target.value) : delete x.name))} />
        </Field>
        <Field needs={need(`squelch-${name}`)} anchor={`squelch-${name}`} label="Squelch, dB above noise" hint="For channels without their own">
          <NumberInput label="Squelch, dB above noise" value={conv.squelchDb} onChange={(v) => edit((x) => void (x.squelchDb = Math.max(3, Math.min(40, v ?? 8))))} />
        </Field>
      </Group>
      <Group title="FCC licenses" hint="Optional: who is licensed on each channel's frequency (they can be different licensees'). Identify looks them up; Motorola P25 channels say theirs on the air." cols={1}>
        <FccTable target={{ kind: "conventional", index: k }} fcc={conv.fcc} />
      </Group>
      {chans.some((c) => c.mode !== "fm" && c.mode !== "am") && (
        <Group title="Talkgroup names" hint="For the talkgroups its P25, DMR and NXDN channels name on the air (a row's own name is for calls filed under its talkgroup).">
          <TalkgroupsField sys={conv} />
        </Group>
      )}
      <Group
        title="Channels"
        hint="Each frequency, with the tone, NAC, colour code or RAN it records. Several rows can share a frequency."
        full
        cols={1}
        actions={
          <>
            {!linked && (
              <button className="btn ghost small" onClick={() => csvRef.current?.click()}>
                Import CSV…
              </button>
            )}
            <button className="btn ghost small" disabled={!chans.length} onClick={exportCsv}>
              Export CSV
            </button>
            {!linked && (
              <button className="btn small" onClick={() => add([{ freqHz: 0, mode: bulkMode, name: "", enabled: true }])}>
                Add a channel
              </button>
            )}
          </>
        }
      >
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
        {linked && (
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
        )}
        {!linked && (
          <Field label="Add frequencies, MHz" hint="Several at once, separated by commas or spaces">
            <div className="row">
              <input className="mono grow" value={bulk} placeholder="154.430, 155.100, 460.125" onChange={(e) => setBulk(e.target.value)} />
              <select value={bulkMode} onChange={(e) => setBulkMode(e.target.value as Channel["mode"])} aria-label="Mode for the added channels">
                {modeOptions}
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
        {chans.length > 0 && (
          <>
            <div className="table-wrap channels-wrap">
              <table className="calls channels">
                <thead>
                  <tr>
                    <th title="Record this channel">On</th>
                    <th>Frequency, MHz</th>
                    <th>Mode</th>
                    <th title="Analog: the channel filter's width">Bandwidth</th>
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
                        {linked ? <span className="mono">{mhzCell(ch.freqHz)}</span> : <DraftInput className="mono" codec={mhz} value={ch.freqHz} placeholder="MHz" aria-label="Frequency, MHz" onChange={(hz) => editRow(i, (x) => void (x.freqHz = hz))} />}
                      </td>
                      <td>
                        <select
                          value={ch.mode}
                          disabled={linked}
                          aria-label="Mode"
                          onChange={(e) =>
                            editRow(i, (x) => {
                              x.mode = e.target.value as Channel["mode"];
                              // A bandwidth is the mode's (AM's and FM's mean different channels); AM has no codes.
                              delete x.bandwidthKhz;
                              if (x.mode === "am") delete x.tone;
                            })
                          }
                        >
                          {modeOptions}
                        </select>
                      </td>
                      <td>
                        <BandwidthSelect
                          ch={ch}
                          disabled={linked}
                          onChange={(khz) =>
                            edit((x) => {
                              // Rows sharing the frequency share it.
                              for (const r of x.channels.filter((o) => Math.abs(o.freqHz - ch.freqHz) < 1)) {
                                if (khz === undefined) delete r.bandwidthKhz;
                                else r.bandwidthKhz = khz;
                              }
                            })
                          }
                        />
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
                        <NumberInput
                          narrow
                          disabled={linked}
                          value={ch.talkgroup}
                          placeholder={ch.freqHz ? String(talkgroups[i]) : "auto"}
                          label="Talkgroup"
                          onChange={(v) =>
                            editRow(i, (x) => {
                              if (v === undefined || v <= 0) delete x.talkgroup;
                              else x.talkgroup = Math.round(v);
                            })
                          }
                        />
                      </td>
                      <td>
                        <NumberInput
                          narrow
                          disabled={linked}
                          value={ch.squelchDb}
                          placeholder={String(conv.squelchDb)}
                          label="Squelch, dB above noise"
                          onChange={(v) =>
                            editRow(i, (x) => {
                              if (v === undefined) delete x.squelchDb;
                              else x.squelchDb = Math.max(3, Math.min(40, v));
                            })
                          }
                        />
                      </td>
                      <td>
                        {!linked && (
                          <span className="row-actions">
                            {ch.freqHz > 0 && ch.mode !== "am" && (
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
            {!linked && (
              <div className="row">
                <span className="spacer" />
                <button
                  className="btn ghost small danger"
                  onClick={() => {
                    if (window.confirm(`Remove all ${chans.length} conventional channels?`)) {
                      ids.current = [];
                      edit((x) => void (x.channels = []));
                    }
                  }}
                >
                  Remove all channels
                </button>
              </div>
            )}
          </>
        )}
        {can("service") && !linked && (
          <Field
            needs={need(`channels-${name}`)}
            anchor={`channels-${name}`}
            label="Channel file (optional)"
            hint="A CSV to edit in a spreadsheet instead, re-read at each start; created from this list if new"
          >
            <div className="row">
              <input className="mono grow" value={filePath} placeholder="channels.csv" onChange={(e) => setFilePath(e.target.value)} />
              <button className="btn" disabled={!filePath.trim()} onClick={() => setChannelFile(k, filePath.trim())}>
                Use this file
              </button>
            </div>
          </Field>
        )}
        <details className="help">
          <summary>CSV format</summary>
          <p className="small">
            A header row, then one channel per row; <b>Export CSV</b> writes one to start from. Columns, in any order: <code>TG Number</code> (empty = the
            frequency in kHz), <code>Frequency</code> (MHz with a decimal point, or Hz), <code>Mode</code> (<code>fm</code>, <code>am</code>, <code>p25</code>,{" "}
            <code>dmr</code>, <code>nxdn48</code> or <code>nxdn96</code>; empty = fm, or what the Tone says: a NAC p25, a colour code dmr, a RAN nxdn48),{" "}
            <code>Tone</code> (the code the row records: a CTCSS tone <code>151.4</code>, a DCS code <code>D023N</code>, a NAC <code>NAC 293</code>, a
            colour code <code>CC1 TS2 TG201</code>, a RAN <code>RAN 5</code>; empty = any), <code>Alpha Tag</code>, <code>Description</code>,{" "}
            <code>Tag</code>, <code>Category</code>, <code>Squelch dB</code> (above the noise; empty = the default), <code>Bandwidth kHz</code> (fm and am: the
            channel filter's width, 4–20; empty = the default), <code>Enable</code> (<code>false</code> =
            off). Trunk Recorder's channel file reads as is; its <code>Squelch</code> column (an absolute level) isn't used.
          </p>
        </details>
      </Group>
      {chans.some((ch) => ch.mode === "fm" || ch.mode === "am") && (
        <DemodGroup
          conv={conv}
          edit={(fn) =>
            edit((x) => {
              x.demod = { ...x.demod };
              fn(x.demod);
              if (!Object.keys(x.demod).length) delete x.demod;
            })
          }
        />
      )}
      {chans.length > 0 && (
        <Group title="Units" hint="Names for the radios heard on these channels." cols={1}>
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
        </Group>
      )}
      {chans.length > 0 && (
        <Group title="Plugins" hint="This system's own plugin settings and delays. Anything left at Default follows the Plugins tab." cols={1}>
          <SystemPluginSettings system={{ conv: k }} />
          <SystemDelays system={{ conv: k }} />
        </Group>
      )}
      {chans.length > 0 && (
        <Group title="Recording" hint="These channels' own call rules and what they keep. Anything left at Default follows the Recording tab." cols={1}>
          <RecordingOverrides
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
        </Group>
      )}
    </Card>
  );
}

/** An analog channel's filter width: the mode's default, or a preset (or what a file set). */
function BandwidthSelect(props: { ch: Channel; disabled: boolean; onChange: (khz: number | undefined) => void }) {
  const { ch } = props;
  if (ch.mode !== "fm" && ch.mode !== "am") return <span className="muted">—</span>;
  const presets = BANDWIDTH_PRESETS[ch.mode];
  const value = ch.bandwidthKhz === undefined ? "" : String(ch.bandwidthKhz);
  const custom = ch.bandwidthKhz !== undefined && !presets.some((p) => p.khz === ch.bandwidthKhz);
  return (
    <select
      value={value}
      disabled={props.disabled}
      aria-label="Bandwidth"
      title={
        ch.mode === "fm"
          ? "The channel filter: narrow for 12.5 kHz channels, wide FM for 25 kHz ones (±5 kHz deviation)."
          : "The channel filter: 8.33 kHz or 25 kHz airband channels, or wide for UHF military air and a transmitter that is off frequency."
      }
      onChange={(e) => props.onChange(e.target.value ? Number(e.target.value) : undefined)}
    >
      <option value="">Default ({defaultBandwidthKhz(ch)} kHz)</option>
      {presets.map((p) => (
        <option key={p.khz} value={p.khz}>
          {p.khz} kHz: {p.label}
        </option>
      ))}
      {custom && <option value={value}>{value} kHz</option>}
    </select>
  );
}

/** A conventional system's demodulation settings, for the analog modes it has; each left at Default is left out. */
function DemodGroup(props: { conv: Conventional; edit: (fn: (d: Demod) => void) => void }) {
  const d = props.conv.demod ?? {};
  const fm = props.conv.channels.some((ch) => ch.mode === "fm");
  const am = props.conv.channels.some((ch) => ch.mode === "am");
  const set = <K extends keyof Demod>(k: K, v: Demod[K] | undefined) =>
    props.edit((x) => {
      if (v === undefined) delete x[k];
      else x[k] = v;
    });
  const clamp = (v: number | undefined, max: number) => (v === undefined ? undefined : Math.max(0, Math.min(max, Math.round(v))));
  return (
    <Group title="Demodulation" hint="How the analog channels become audio. Each channel's bandwidth is in its row. Default suits most channels." cols={3}>
      {fm && (
        <Field label="FM de-emphasis" hint="Off for channels sent without pre-emphasis: they sound muffled with it">
          <select value={d.fmDeemphasis === false ? "off" : ""} onChange={(e) => set("fmDeemphasis", e.target.value === "off" ? false : undefined)}>
            <option value="">Default (750 µs)</option>
            <option value="off">Off</option>
          </select>
        </Field>
      )}
      {am && (
        <Field label="AM level" hint="How fast the volume follows a fading carrier: Fast keeps an aircraft steady, Slow pumps less on a steady signal">
          <select value={d.amLevel ?? ""} onChange={(e) => set("amLevel", e.target.value === "slow" ? "slow" : undefined)}>
            <option value="">Default (Fast, 50 ms)</option>
            <option value="slow">Slow (0.5 s)</option>
          </select>
        </Field>
      )}
      {am && (
        <Field label="AM low cut, Hz" hint="Takes carrier hum and flutter from under the voice. 0: none">
          <NumberInput label="AM low cut, Hz" value={d.amLowCutHz} placeholder="200" onChange={(v) => set("amLowCutHz", clamp(v, AM_LOW_CUT_MAX_HZ))} />
        </Field>
      )}
      <Field label="Squelch open delay, ms" hint="The carrier must hold this long, so a neighbour's splatter doesn't open it">
        <NumberInput label="Squelch open delay, ms" value={d.squelchOpenMs} placeholder="30" onChange={(v) => set("squelchOpenMs", clamp(v, SQUELCH_OPEN_MAX_MS))} />
      </Field>
      <Field label="Squelch hang, ms" hint="Bridges fades on weak signals; the noise after a transmission is still cut. Live audio runs this much later">
        <NumberInput label="Squelch hang, ms" value={d.squelchHangMs} placeholder="2" onChange={(v) => set("squelchHangMs", clamp(v, SQUELCH_HANG_MAX_MS))} />
      </Field>
    </Group>
  );
}

/** The site-lock inputs, in order. */
const LOCK_INPUTS: { key: keyof SiteIdentity; label: string; hex: boolean }[] = [
  { key: "nac", label: "NAC", hex: true },
  { key: "wacn", label: "WACN", hex: true },
  { key: "sysId", label: "System ID", hex: true },
  { key: "rfss", label: "RFSS", hex: false },
  { key: "site", label: "Site", hex: false },
];

function DmrFields(props: { site: Site; edit: (fn: (x: Site) => void) => void }) {
  const { site, edit } = props;
  return (
    <>
      <Field label="Voice frequencies, MHz" hint="Tier III, Capacity Max, Connect Plus" wide>
        <DraftInput
          className="mono"
          codec={mhzList}
          value={site.dmrChannelsHz ?? []}
          placeholder="452.275, 452.300"
          onChange={(list) =>
            edit((x) => {
              if (list.length) x.dmrChannelsHz = list;
              else delete x.dmrChannelsHz;
            })
          }
        />
      </Field>
      <Field label="Colour code">
        <NumberInput
          label="Colour code"
          value={site.colorCode}
          placeholder="auto"
          onChange={(v) =>
            edit((x) => {
              if (v !== undefined && Number.isInteger(v) && v >= 0 && v <= 15) x.colorCode = v;
              else delete x.colorCode;
            })
          }
        />
      </Field>
      <LcnTableField site={site} edit={edit} label="Channel table (optional)" hint="Channel = MHz, e.g. 101=452.275" placeholder="learned from the air" />
    </>
  );
}

function NxdnFields(props: { sys: System; site: Site; edit: (fn: (x: Site) => void) => void }) {
  const { sys, site, edit } = props;
  const typeD = sys.nxdnType === "typeD";
  return (
    <>
      <Field label="Rate" hint={typeD ? "The repeaters'" : "The control channel's"}>
        <select
          value={site.nxdnRate === "nxdn96" ? "nxdn96" : "nxdn48"}
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
        <NumberInput
          label="RAN"
          value={site.ran}
          placeholder="auto"
          onChange={(v) =>
            edit((x) => {
              if (v !== undefined && Number.isInteger(v) && v >= 0 && v <= 63) x.ran = v;
              else delete x.ran;
            })
          }
        />
      </Field>
      <Field label={typeD ? "More repeaters, MHz" : "Voice frequencies, MHz"} hint={typeD ? "Optional: may all go above" : "Watched; channel numbers are learned from them"} wide>
        <DraftInput
          className="mono"
          codec={mhzList}
          value={site.nxdnChannelsHz ?? []}
          placeholder="451.0125, 451.0375"
          onChange={(list) =>
            edit((x) => {
              if (list.length) x.nxdnChannelsHz = list;
              else delete x.nxdnChannelsHz;
            })
          }
        />
      </Field>
      <LcnTableField
        site={site}
        edit={edit}
        label="Channel table (optional)"
        hint={typeD ? "Repeater number = MHz, e.g. 1=451.0125" : "Channel number = MHz, e.g. 20=451.0125"}
        placeholder={typeD ? "none" : "learned from the air"}
      />
      {!typeD && (
        <p className="muted small" style={{ gridColumn: "1 / -1", margin: 0 }}>
          A Type-C control channel grants channel numbers. Their frequencies come from the channel table (RadioReference lists it for many systems), from the site if it
          uses Direct Frequency Assignment, or are learned when the voice frequencies are listed above.
        </p>
      )}
    </>
  );
}

/** A site's `lcnTableHz`: channel number = MHz (DMR, NXDN, EDACS). */
function LcnTableField(props: { site: Site; edit: (fn: (x: Site) => void) => void; label: string; hint: string; placeholder: string }) {
  const { site, edit } = props;
  return (
    <Field label={props.label} hint={props.hint} wide>
      <DraftInput
        className="mono"
        codec={lcnTable}
        value={site.lcnTableHz ?? {}}
        placeholder={props.placeholder}
        onChange={(t) =>
          edit((x) => {
            if (Object.keys(t).length) x.lcnTableHz = t;
            else delete x.lcnTableHz;
          })
        }
      />
    </Field>
  );
}

/** An EDACS site's channel list (LCN → frequency) and ESK. */
function EdacsFields(props: { site: Site; edit: (fn: (x: Site) => void) => void }) {
  const { site, edit } = props;
  return (
    <>
      <LcnTableField site={site} edit={edit} label="Channels (LCNs)" hint="LCN = MHz, e.g. 1=856.1875, 3=857.7125" placeholder="1=856.1875, 2=…" />
      <Toggle
        label="ESK 0xA0"
        hint="Some Harris systems scramble the control channel"
        checked={!!site.esk}
        onChange={(v) =>
          edit((x) => {
            if (v) x.esk = true;
            else delete x.esk;
          })
        }
      />
      <p className="muted small" style={{ gridColumn: "1 / -1", margin: 0 }}>
        The control channel grants calls a channel number (LCN); this list (RadioReference has it) turns it into a frequency. Every listed frequency is watched, so a list
        with its numbers mixed up is put right from the air. A call on an LCN whose frequency isn't listed is logged and not recorded.
      </p>
    </>
  );
}

/** A SmartNet site's band plan (Trunk Recorder's settings). */
function SmartnetFields(props: { site: Site; edit: (fn: (x: Site) => void) => void }) {
  const { site, edit } = props;
  const custom = (site.bandplan ?? "").startsWith("400") || site.bandplan?.toLowerCase() === "obt";
  const setNum = (k: "bandplanBaseHz" | "bandplanSpacingHz" | "bandplanOffset" | "bandplanHighHz", v: number | undefined) =>
    edit((x) => {
      if (v === undefined || v < 0) delete x[k];
      else x[k] = v;
    });
  return (
    <>
      <Field label="Band plan">
        <select value={custom ? "400_custom" : (site.bandplan ?? "800_standard")} onChange={(e) => edit((x) => void (x.bandplan = e.target.value))}>
          <option value="800_standard">800 MHz standard</option>
          <option value="800_reband">800 MHz rebanded</option>
          <option value="800_splinter">800 MHz splinter</option>
          <option value="900">900 MHz</option>
          <option value="400_custom">VHF / UHF (custom, OBT)</option>
        </select>
      </Field>
      {custom && (
        <div className="id-grid wide">
          <Field label="Base, Hz" hint="Frequency of the offset channel">
            <NumberInput label="Band plan base" placeholder="489087500" value={site.bandplanBaseHz} onChange={(v) => setNum("bandplanBaseHz", v)} />
          </Field>
          <Field label="Spacing, Hz">
            <NumberInput label="Band plan spacing" placeholder="25000" value={site.bandplanSpacingHz} onChange={(v) => setNum("bandplanSpacingHz", v)} />
          </Field>
          <Field label="Offset (channel)">
            <NumberInput label="Band plan offset" placeholder="380" value={site.bandplanOffset} onChange={(v) => setNum("bandplanOffset", v)} />
          </Field>
          <Field label="High, Hz" hint="Highest outbound channel">
            <NumberInput label="Band plan high" placeholder="496612500" value={site.bandplanHighHz} onChange={(v) => setNum("bandplanHighHz", v)} />
          </Field>
        </div>
      )}
    </>
  );
}

const SYSTEM_TYPES: Record<System["type"], string> = { p25: "P25", smartnet: "SmartNet / SmartZone", dmr: "DMR (trunked)", nxdn: "NXDN (trunked)", edacs: "EDACS (analog / ProVoice)" };

/** A system's own settings, as a confirm names them: making two systems one keeps the first's. */
const SYSTEM_OWN: [keyof System, string][] = [
  ["unitNames", "unit names"],
  ["recording", "recording rules"],
  ["plugins", "plugin settings"],
  ["pluginDelays", "plugin delays"],
  ["keys", "keys"],
  ["defaultMode", "default mode"],
];

/** Whether a site's control channels and voice channels land inside a source: chips. */
function SiteCoverage(props: { c: Config; sys: System; sites: Site[] }) {
  const { c, sys } = props;
  const sites = sys.enabled ? props.sites.filter((x) => x.enabled) : [];
  const ccs = sites.flatMap((x) => x.controlChannelsHz);
  const voice = sites.flatMap((x) => x.voiceChannelsHz);
  if (!ccs.length && !voice.length) return null;
  const centers = resolvedCenters(c);
  const ccOn = [...new Set(ccs.map((f) => sourceCovering(c, centers, f)).filter((k) => k >= 0))];
  const voiceIn = voice.filter((f) => sourceCovering(c, centers, f) >= 0).length;
  return (
    <>
      {ccs.length > 0 &&
        (ccOn.length ? (
          <span className="chip ok">control channel on source {ccOn.map((k) => k + 1).join(", ")}</span>
        ) : (
          <span className="chip bad">no control channel inside a source</span>
        ))}
      {voice.length > 0 && (
        <span className={`chip ${voiceIn === voice.length ? "ok" : "warn"}`} title="Voice channels found by the survey, inside a source">
          voice {voiceIn}/{voice.length} covered
        </span>
      )}
    </>
  );
}

/** One site of system `i`: folded, its name, Record switch, control channels and coverage; open, how it is followed. */
function SiteCard(props: { c: Config; i: number; k: number; onRemoveSystem: () => void }) {
  const { c, i, k } = props;
  const sys = c.systems[i];
  const site = sys.sites[k];
  const need = useNeed();
  const edit = (fn: (x: Site) => void) => updateConfig((x) => fn(x.systems[i].sites[k]));
  const setExpect = (f: keyof SiteIdentity, v: number | undefined) =>
    edit((x) => {
      x.expect = { ...x.expect };
      if (v === undefined) delete x.expect[f];
      else x.expect[f] = v;
    });
  const name = siteNameOf(sys, site);
  const dupName = sys.sites.some((x, j) => j !== k && siteNameOf(sys, x) === name);
  // The lock, on the fields this protocol states (others are ignored).
  const lockFields = siteLockFields(sys);
  const lockedTo: SiteIdentity = Object.fromEntries(lockFields.filter((f) => site.expect[f] != null).map((f) => [f, site.expect[f]]));
  const locked = Object.keys(lockedTo).length > 0;
  // Its to-dos' fields: `site-<system>-<k>`, `cc-<system>-<k>`.
  const at = `${sys.shortName}-${k}`;
  const todo = openTodos(useApp()).filter((t) => t.target === `site-${at}` || t.target === `cc-${at}`).length;
  const typeD = sys.type === "nxdn" && sys.nxdnType === "typeD";
  const remove = () => {
    if (sys.sites.length === 1) return props.onRemoveSystem();
    if (!confirm(`Remove the site ${name} from ${sys.shortName}?`)) return;
    updateConfig((x) => void x.systems[i].sites.splice(k, 1));
  };
  return (
    <Card
      cardKey={siteKey(sys, site)}
      // Sites start folded.
      n={2}
      className={`site-card${site.enabled ? "" : " off"}`}
      head={
        <>
          <div className="set-card-title">
            <h3>{name}</h3>
            <span className="set-card-sub mono">
              {site.controlChannelsHz.length ? `${site.controlChannelsHz.map((f) => formatMhz(f)).join(", ")} MHz` : "no control channel"}
              {locked ? ` · ${siteText(lockedTo)}` : ""}
            </span>
          </div>
          <div className="row set-card-chips">
            {todo > 0 && <span className="chip warn">{todo} to finish</span>}
            <SiteCoverage c={c} sys={sys} sites={[site]} />
          </div>
        </>
      }
      actions={
        <>
          <Switch label="Record" checked={site.enabled} onChange={(v) => edit((x) => void (x.enabled = v))} />
          <button className="btn ghost small" onClick={remove}>
            Remove
          </button>
        </>
      }
    >
      <div className="set-group-body cols-2 site-body">
        <Field label="Site name" hint={dupName ? "Another site has it — each needs its own" : "As the talkgroup file's Preferred Site names it"}>
          <input
            value={site.name ?? ""}
            placeholder={sys.shortName}
            onChange={(e) =>
              edit((x) => {
                if (e.target.value.trim()) x.name = e.target.value;
                else delete x.name;
              })
            }
          />
        </Field>
        {sys.type !== "dmr" && sys.type !== "nxdn" && sys.type !== "edacs" && (
          <Field label={sys.type === "smartnet" ? "P25 voice modulation" : "Modulation"}>
            <select value={site.modulation} onChange={(e) => edit((x) => void (x.modulation = e.target.value as Site["modulation"]))}>
              <option value="auto">Detect (then only that receiver)</option>
              <option value="fsk4">C4FM (fsk4)</option>
              <option value="qpsk">CQPSK / LSM simulcast (qpsk)</option>
            </select>
          </Field>
        )}
        <Field
          needs={need(`cc-${at}`)}
          anchor={`cc-${at}`}
          label={sys.type === "dmr" ? "Site frequencies, MHz" : typeD ? "Repeaters, MHz" : "Control channels, MHz"}
          hint={
            sys.type === "dmr"
              ? "Capacity Plus: every repeater. Others: control channels."
              : typeD
                ? "Every repeater of the site"
                : sys.type === "edacs"
                  ? "The site's control channel; list more (or every channel) if it moves"
                  : "Later ones are fallbacks"
          }
          wide
        >
          <DraftInput
            className="mono"
            codec={mhzList}
            value={site.controlChannelsHz}
            placeholder="857.9875, 858.9875"
            onChange={(list) => edit((x) => void (x.controlChannelsHz = list))}
          />
        </Field>
        {sys.type === "smartnet" && <SmartnetFields site={site} edit={edit} />}
        {sys.type === "dmr" && <DmrFields site={site} edit={edit} />}
        {sys.type === "nxdn" && <NxdnFields sys={sys} site={site} edit={edit} />}
        {sys.type === "edacs" && <EdacsFields site={site} edit={edit} />}
        {lockFields.length > 0 && (
          <details className={`help wide${need(`site-${at}`) ? " needs" : ""}`} id={`need-site-${at}`} open={need(`site-${at}`) ? true : undefined}>
            <summary>
              Site lock{locked ? <span className="muted"> — only {siteText(lockedTo)}</span> : <span className="muted"> — off</span>}
            </summary>
            <p className="muted small">
              {need(`site-${at}`) && (
                <>
                  <span className="field-needs">{need(`site-${at}`)}</span>{" "}
                </>
              )}
              Follow only a control channel announcing this site, not a neighbour. Empty = any;{" "}
              {sys.type === "edacs" ? "the site number as the control channel says it" : sys.type === "smartnet" || sys.type === "nxdn" ? "System ID in hex" : "NAC, WACN and System ID in hex"}.
            </p>
            <div className="id-grid">
              {LOCK_INPUTS.filter((x) => lockFields.includes(x.key)).map((x) => (
                <Field key={x.key} label={x.label}>
                  <NumberInput label={x.label} hex={x.hex} value={site.expect[x.key] ?? undefined} onChange={(v) => setExpect(x.key, v)} />
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
    </Card>
  );
}

function SystemCard(props: { c: Config; i: number }) {
  const { c, i } = props;
  const sys = c.systems[i];
  const need = useNeed();
  const app = useApp();
  const plugins = app.plugins?.plugins ?? [];
  const edit = (fn: (x: System) => void) => updateConfig((x) => fn(x.systems[i]));
  const color = systemColor(c, sys.shortName);
  const dupName = c.systems.some((x, k) => k !== i && x.shortName === sys.shortName);
  // Its to-dos on this tab, its sites' among them.
  const todo = openTodos(app).filter((t) => "system" in t.item && t.item.system === sys.shortName && tabOf(t.target) === "systems").length;
  // Systems set up apart that announce this one on the air.
  const same = sameOnTheAir(c, app.status?.systems ?? []).flatMap(([a, b]) => (a === sys ? [b] : b === sys ? [a] : []));

  const remove = (lastSite = false) => {
    if (!confirm(`${lastSite ? `This is ${sys.shortName}'s only site. ` : ""}Remove ${sys.shortName}? Its recordings stay on disk.`)) return;
    updateConfig((x) => {
      x.systems.splice(i, 1);
      forgetTalkgroups(x, sys.shortName);
    });
  };
  const addSite = () => {
    edit((x) => {
      const n = newSiteOf(x, {});
      x.sites.push(n);
      setCardOpen(siteKey(x, n), true);
    });
  };
  // The other system's sites move here (each named as its system was, unless it has a name); this one's settings stay.
  const join = (other: System) => {
    const j = c.systems.indexOf(other);
    const dropped = SYSTEM_OWN.filter(([k]) => other[k] !== undefined && other[k] !== "" && JSON.stringify(other[k]) !== JSON.stringify(sys[k])).map(([, w]) => w);
    // Its talkgroup file comes along if this one has none.
    const [theirs, mine] = [app.talkgroups[other.shortName], app.talkgroups[sys.shortName]];
    if (theirs && mine && theirs !== mine) dropped.push("talkgroup file");
    const n = other.sites.length;
    const ask =
      `Make ${other.shortName} a site${n === 1 ? "" : "s"} of ${sys.shortName}? ` +
      `Its ${n === 1 ? "site moves" : `${n} sites move`} here and ${other.shortName} is removed; its recordings stay on disk.` +
      (dropped.length ? ` Its ${dropped.join(", ")} ${dropped.length === 1 ? "is" : "are"} dropped: ${sys.shortName}'s are kept.` : "");
    if (j < 0 || !confirm(ask)) return;
    updateConfig((x) => {
      const to = x.systems[i];
      const from = x.systems[j];
      for (const s of from.sites) {
        const taken = new Set(to.sites.map((y) => siteNameOf(to, y)));
        const want = s.name?.trim() || from.shortName;
        to.sites.push({ ...s, name: taken.has(want) ? newSiteName(to) : want, enabled: s.enabled && from.enabled });
      }
      if (from.enabled) to.enabled = true;
      // Plugin settings that named it name this one.
      renameSystemRefs(x, from.shortName, to.shortName, plugins);
      if (!mine) moveTalkgroups(from.shortName, to.shortName);
      x.systems.splice(j, 1);
      forgetTalkgroups(x, from.shortName);
    });
    setNotice(`${other.shortName} is a part of ${sys.shortName} now.`);
  };

  return (
    <Card
      cardKey={sysKey(sys, i, c.systems)}
      n={c.systems.length}
      className={`system-card${sys.enabled ? "" : " off"}`}
      style={{ ["--sys-color" as string]: color }}
      id={`need-sys-${sys.shortName}`}
      head={
        <>
          <span className="sys-dot" style={{ background: color }} />
          <div className="set-card-title">
            <h3>{sys.shortName || "(no name)"}</h3>
            <span className="set-card-sub">
              {sys.name?.trim() ? `${sys.name.trim()} · ` : ""}
              {SYSTEM_TYPES[sys.type]}
            </span>
          </div>
          <div className="row set-card-chips">
            {todo > 0 && <span className="chip warn">{todo} to finish</span>}
            {sys.sites.length > 1 && <span className="chip">{sys.sites.length} sites</span>}
            <SiteCoverage c={c} sys={sys} sites={sys.sites} />
          </div>
        </>
      }
      actions={
        <>
          <Switch label="Record" checked={sys.enabled} onChange={(v) => edit((x) => void (x.enabled = v))} />
          <IdentifyFcc target={{ kind: "system", index: i }} what={sys.name || sys.shortName} freqs={siteFrequencies(sys.sites).map((x) => x.freqHz)} fcc={sys.fcc} />
          <button className="btn ghost small" onClick={() => remove()}>
            Remove
          </button>
        </>
      }
      note={
        same.length > 0 && (
          <div className="set-card-needs">
            {same.map((other) => (
              <div key={other.shortName} className="banner same-system">
                <span>
                  <b>{other.shortName}</b> announces the same system on the air.
                </span>
                <button className="btn small" onClick={() => join(other)}>
                  Make them one system
                </button>
              </div>
            ))}
          </div>
        )
      }
    >
      <Group title="Basics" hint="The short name is its calls' folder.">
        <Field label="Short name" hint={dupName ? "Already used — each needs its own folder" : undefined}>
          <input
            value={sys.shortName}
            onChange={(e) => {
              const to = cleanShortName(e.target.value);
              updateConfig((x) => renameSystem(x, x.systems[i], to));
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
                x.type = systemType(e.target.value);
                if (x.type === "smartnet") for (const y of x.sites) y.bandplan ??= "800_standard";
              })
            }
          >
            {Object.entries(SYSTEM_TYPES).map(([k, label]) => (
              <option key={k} value={k}>
                {label}
              </option>
            ))}
          </select>
        </Field>
        {sys.type === "smartnet" && (
          <Field label="Voice of unknown talkgroups" hint="Until a grant says otherwise">
            <select value={sys.defaultMode ?? "digital"} onChange={(e) => edit((x) => void (e.target.value === "analog" ? (x.defaultMode = "analog") : delete x.defaultMode))}>
              <option value="digital">P25</option>
              <option value="analog">Analog FM</option>
            </select>
          </Field>
        )}
        {sys.type === "nxdn" && (
          <Field label="NXDN type">
            <select
              value={sys.nxdnType === "typeD" ? "typeD" : "typeC"}
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
        )}
      </Group>
      <Group
        title="Sites"
        hint="Where the recorder finds it: each site's control channel. A call heard on several sites is saved once."
        actions={
          <button className="btn small" onClick={addSite}>
            Add site
          </button>
        }
        cols={1}
      >
        <div className="sites-list">
          {sys.sites.map((x, k) => (
            <SiteCard key={siteKey(sys, x)} c={c} i={i} k={k} onRemoveSystem={() => remove(true)} />
          ))}
          {!sys.sites.length && <p className="muted small">No sites: add one with its control channel.</p>}
        </div>
      </Group>
      <Group title="FCC licenses" hint="Optional: who is licensed on each frequency. Motorola P25 sites say their call signs on the air; Identify looks licensees up. Plugins are told them." cols={1}>
        <FccTable target={{ kind: "system", index: i }} fcc={sys.fcc} />
      </Group>
      <Group title="Talkgroups and units" hint="Names for what is heard, and which talkgroups are recorded.">
        <TalkgroupsField sys={sys} needs={need(`tg-${sys.shortName}`)} anchor={`tg-${sys.shortName}`} />
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
      </Group>
      <Group title="Plugins" hint="This system's own plugin settings and delays. Anything left at Default follows the Plugins tab." cols={1}>
        <SystemPluginSettings system={i} />
        <SystemDelays system={i} />
      </Group>
      <Group title="Recording" hint="This system's own call rules and what it keeps. Anything left at Default follows the Recording tab." cols={1}>
        <RecordingOverrides
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
      </Group>
    </Card>
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
      <FileInput
        inputRef={ref}
        accept=".csv,text/csv,text/plain"
        onFile={async (f) => {
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
    <section className="set-card">
      <header className="set-card-head">
        <div className="set-card-title">
          <h3>Log</h3>
          <span className="set-card-sub">Applied at once</span>
        </div>
      </header>
      <Group title="Where it goes">
        <Toggle label="To the console" hint="Standard error (stderr)" checked={l.console} onChange={(v) => set({ console: v })} />
        <Toggle label="To the system log" checked={l.syslog} onChange={(v) => set({ syslog: v })} />
        <Toggle label="To files" hint="A new one daily and at 100 MB" checked={l.file} onChange={(v) => set({ file: v })} />
        {l.file && (
          <>
            <Field label="Folder" hint="Relative to the config file">
              <input className="mono" value={l.dir} placeholder="logs" onChange={(e) => set({ dir: e.target.value })} />
            </Field>
            <Toggle
              label="One file, for logrotate"
              hint="trunk-pro.log, never rotated here; SIGHUP reopens it"
              checked={l.syslogFriendly}
              onChange={(v) => set({ syslogFriendly: v })}
            />
          </>
        )}
      </Group>
      <Group
        title="What it says"
        hint={
          <>
            <code>--log-level</code> on the command line overrides Level.
          </>
        }
      >
        <Field label="Level" hint="Debug adds unrecorded calls; trace, every control message">
          <select value={l.level} onChange={(e) => set({ level: e.target.value as LogSettings["level"] })}>
            {["trace", "debug", "info", "warning", "error", "fatal"].map((v) => (
              <option key={v} value={v}>
                {v}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Decode rate warning, msg/s" hint="Fewer is logged as an error; −1 always logs it">
          <NumberInput label="Decode rate warning" value={l.controlWarnRatePerS} onChange={(v) => set({ controlWarnRatePerS: v ?? 10 })} />
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
        <Toggle label="States as words" hint="“Monitoring”, not a number" checked={l.statusAsString} onChange={(v) => set({ statusAsString: v })} />
      </Group>
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
  | {
      key: BoolRule;
      kind: "bool";
      label: string;
      hint?: string;
      service?: boolean;
      /** Why it does nothing with these settings (greyed out), if it doesn't. */
      inactive?: (r: RecordingRules) => string | undefined;
      /** What the plugins on (those taking an .m4a, by name) make of it, if anything. */
      note?: (m4aPlugins: string[]) => string | undefined;
    }
  | { key: NumRule; kind: "num"; label: string; hint?: string; min: number; max: number; zero?: string }
  | { key: "filenameFormat"; kind: "text"; label: string };

const RULE_GROUPS: { title: string; hint?: string; rules: Rule[] }[] = [
  {
    title: "What is recorded",
    hint: "Which calls are picked up, and when one ends.",
    rules: [
      { key: "recordUnknown", kind: "bool", label: "Talkgroups not in the talkgroup file" },
      { key: "recordUnitToUnit", kind: "bool", label: "Unit-to-unit calls", hint: "private calls between two radios" },
      { key: "callTimeoutS", kind: "num", label: "Call timeout, s", hint: "Silence that ends a call", min: 1, max: 30 },
    ],
  },
  {
    title: "Which calls are kept",
    hint: "Calls too short to be worth keeping, and long ones split.",
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
    hint: "Loudness, for calls that sound alike.",
    rules: [
      { key: "normalizeAudio", kind: "bool", label: "Even out call loudness" },
      { key: "digitalLevelDb", kind: "num", label: "Digital Level Adjustment, dB", min: -20, max: 20 },
      { key: "analogLevelDb", kind: "num", label: "Analog Level Adjustment, dB", min: -20, max: 20 },
    ],
  },
  {
    title: "Files",
    hint: "Where each call is saved.",
    rules: [{ key: "filenameFormat", kind: "text", label: "Folders and file names" }],
  },
  {
    // What stays in the recordings folder once the plugins are done with a call (docs/guides/recordings.md).
    title: "Archive",
    hint: "What stays in the recordings folder once the plugins are done with a call.",
    rules: [
      { key: "archiveWav", kind: "bool", label: "Keep .wav", hint: "off: deleted once the plugins are done with the call", service: true },
      {
        key: "archiveM4a",
        kind: "bool",
        label: "Keep .m4a",
        hint: "a tenth the size of the WAV; needs ffmpeg or afconvert",
        service: true,
        // (An .m4a is made for a plugin that takes one either way: this is only whether it stays.)
        note: (m) => (m.length ? `${m.join(", ")} ${m.length > 1 ? "get" : "gets"} one either way; off: deleted once the plugins are done` : undefined),
      },
      { key: "callLog", kind: "bool", label: "Keep call JSON", hint: "off: deleted once the plugins are done with the call", service: true },
      {
        key: "archiveFilesOnFailure",
        kind: "bool",
        label: "Keep when a plugin fails",
        hint: "keeps every file of the call when any plugin reports a failure",
        service: true,
        inactive: (r) => (r.archiveWav && r.archiveM4a && r.callLog ? "everything is kept anyway" : undefined),
      },
    ],
  },
];

const ruleShown = (r: Rule) => !(r.kind === "bool" && r.service && !can("service"));

function ruleValue(r: Rule, v: Recording[keyof Recording] | undefined): string {
  if (r.kind === "bool") return v ? "on" : "off";
  if (r.kind === "text") return (v as string) || "the usual layout";
  return v === 0 && r.zero ? r.zero : String(v);
}

/** The call rules for the whole recorder (the Recording tab). */
function CallRules(props: { c: Config }) {
  const r = props.c.recording;
  const m4a = useM4aPlugins().map((p) => p.manifest!.name);
  const set = <K extends keyof RecordingRules>(k: K, v: RecordingRules[K]) => updateConfig((x) => void ((x.recording as RecordingRules)[k] = v));
  return (
    <section className="set-card">
      {RULE_GROUPS.filter((g) => g.rules.some(ruleShown)).map((g) => (
        <Group key={g.title} title={g.title} hint={g.hint}>
          {g.rules.filter(ruleShown).map((rule) =>
            rule.kind === "bool" ? (
              <Toggle
                key={rule.key}
                label={rule.label}
                hint={rule.inactive?.(r) ?? rule.note?.(m4a) ?? rule.hint}
                checked={!!r[rule.key]}
                disabled={!!rule.inactive?.(r)}
                onChange={(v) => set(rule.key, v)}
              />
            ) : rule.kind === "num" ? (
              <Field key={rule.key} label={rule.label} hint={rule.hint}>
                <NumberInput
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
                root={can("service") ? props.c.recording.captureDir : undefined}
                onChange={(v) => set("filenameFormat", v.trim() ? v : "")}
              />
            ),
          )}
          {g.title === "Archive" && (!(r.archiveWav || r.archiveM4a) || !r.callLog) && !Object.values(props.c.plugins ?? {}).some((p) => p.enabled) && (
            <p className="muted small wide">
              No plugin is on, so there's nothing to wait for: {!(r.archiveWav || r.archiveM4a) && !r.callLog ? "calls aren't saved at all" : !r.callLog ? "calls' JSON isn't saved at all" : "calls' audio isn't saved at all"}.
            </p>
          )}
        </Group>
      ))}
    </section>
  );
}

/** The Recording tab's groups a system's own rules are in, by its card's two overrides. */
const ARCHIVE_GROUPS = ["Files", "Archive"];

type OverrideProps = { c: Config; value: RecordingOverride | undefined; onChange: (fn: (o: RecordingOverride) => void) => void };

/**
 * A system's own call rules (or the conventional channels'), in two: what's
 * recorded and kept (Recording), and where its files go and which stay
 * (Archive). Each setting left at Default follows the Recording tab.
 */
function RecordingOverrides(props: OverrideProps) {
  return (
    <>
      <RecordingOverridePanel {...props} title="Recording" groups={RULE_GROUPS.filter((g) => !ARCHIVE_GROUPS.includes(g.title))} />
      <RecordingOverridePanel {...props} title="Archive" groups={RULE_GROUPS.filter((g) => ARCHIVE_GROUPS.includes(g.title))} />
    </>
  );
}

function RecordingOverridePanel(props: OverrideProps & { title: string; groups: typeof RULE_GROUPS }) {
  const { c, value, onChange } = props;
  const own = value ?? {};
  const base = c.recording;
  const set = (k: keyof RecordingOverride, v: RecordingOverride[keyof RecordingOverride] | undefined) =>
    onChange((o) => {
      if (v === undefined) delete o[k];
      else (o as Record<string, unknown>)[k] = v;
    });
  const rules = props.groups.flatMap((g) => g.rules).filter(ruleShown);
  const m4a = useM4aPlugins().map((p) => p.manifest!.name);
  const mine = rules.filter((r) => own[r.key] !== undefined);
  // The settings this system ends up with (its own, else the Recording tab's).
  const effective = { ...base, ...own } as RecordingRules;
  return (
    <details className="help override">
      <summary>
        {props.title}
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
                root={can("service") ? base.captureDir : undefined}
                onChange={(v) => set("filenameFormat", v.trim() ? v : undefined)}
              />
            );
          return (
            <Field
              key={rule.key}
              label={rule.label}
              hint={
                (rule.kind === "bool" && rule.inactive?.(effective)) ||
                [rule.kind === "bool" && rule.note?.(m4a), mineNow && `Default: ${ruleValue(rule, base[rule.key])}`].filter(Boolean).join(" · ") ||
                undefined
              }
            >
              {rule.kind === "bool" ? (
                <select
                  className={mineNow ? "overridden" : ""}
                  disabled={!!rule.inactive?.(effective)}
                  value={own[rule.key] === undefined ? "" : own[rule.key] ? "on" : "off"}
                  onChange={(e) => set(rule.key, e.target.value === "" ? undefined : e.target.value === "on")}
                >
                  <option value="">Default ({ruleValue(rule, base[rule.key])})</option>
                  <option value="on">On</option>
                  <option value="off">Off</option>
                </select>
              ) : (
                <NumberInput
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
      {TABS.filter((t) => t.id !== "plugins" || can("plugins")).map((t) => {
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
          <Section
            title="Trunked systems"
            hint="P25, SmartNet, DMR and NXDN systems, followed through their control channels."
            actions={
              <>
                <ExpandAll keys={c.systems.map(sysKey)} />
                <button className="btn ghost" onClick={() => openGuide("import")}>
                  Import Trunk Recorder config…
                </button>
                <button
                  className="btn"
                  onClick={() => {
                    updateConfig((x) => {
                      const n = newSystem(x);
                      x.systems.push(n);
                      addCard(c.systems.map(sysKey), sysKey(n, x.systems.length - 1, x.systems));
                      // Its site open too, for its control channel.
                      setCardOpen(siteKey(n, n.sites[0]), true);
                    });
                  }}
                >
                  Add a system
                </button>
              </>
            }
          >
            {c.systems.map((x, i) => (
              <SystemCard key={sysKey(x, i, c.systems)} c={c} i={i} />
            ))}
            {c.systems.length === 0 && <p className="set-empty">No trunked systems yet. Add one, import a Trunk Recorder config, or find one on the air below.</p>}
          </Section>
          <SurveyPanel c={c} />
        </>
      )}

      {tab === "conventional" && <ConventionalTab c={c} />}

      {tab === "radios" && (
        <Section
          title="Radios"
          hint="Each source hears a slice of spectrum; the systems and channels inside it are recorded."
          actions={
            <>
              <ExpandAll keys={c.sources.map(srcKey)} />
              <button
                className="btn"
                onClick={() => {
                  setFreshSource(c.sources.length);
                  updateConfig((x) => {
                    const n = newDongle();
                    x.sources.push(n);
                    addCard(c.sources.map(srcKey), srcKey(n, x.sources.length - 1, x.sources));
                  });
                }}
              >
                Add a source
              </button>
            </>
          }
        >
          {c.sources.map((x, i) => (
            <SourceCard key={srcKey(x, i, c.sources)} c={c} i={i} fresh={freshSource === i} onFreshDone={() => setFreshSource(null)} />
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
                  <b>Linux:</b> add a udev rule giving your user access to USB 0bda:2838 and 0bda:2832, and unload <code>dvb_usb_rtl28xxu</code>.
                </li>
                <li>For several dongles or long unattended runs, the desktop app is sturdier.</li>
              </ul>
            ) : (
              <ul>
                <li>
                  <b>Windows:</b> install the WinUSB driver for “Bulk-In, Interface 0” with Zadig.
                </li>
                <li>
                  <b>Linux:</b> add a udev rule giving your user access to USB 0bda:2838 and 0bda:2832, and unload the DVB driver (<code>sudo rmmod dvb_usb_rtl28xxu</code>) if it holds the dongle.
                </li>
                <li>
                  <b>macOS:</b> works as-is. Quit other SDR apps that hold the dongle.
                </li>
              </ul>
            )}
          </details>
        </Section>
      )}

      {tab === "recording" && (
        <Section title="Recorder" hint="Where calls go and how the recorder runs.">
          <section className="set-card">
            <Group title="Storage" hint={web ? undefined : "The RAM spool spares an SSD the writes of calls that are uploaded, then deleted."}>
              {web ? (
                <p className="muted small wide">Calls are kept in this browser; <i>Recorded calls</i> exports them.</p>
              ) : (
                <>
                  <Field label="Recordings folder" wide>
                    <input className="mono" value={c.recording.captureDir} onChange={(e) => updateConfig((x) => void (x.recording.captureDir = e.target.value))} />
                  </Field>
                  <Toggle
                    label="Keep calls waiting to upload in memory"
                    hint="A RAM disk, not the recordings folder; from the next start"
                    checked={c.recording.ramSpool?.enabled ?? false}
                    onChange={(v) => updateConfig((x) => void (x.recording.ramSpool = { sizeMb: 256, ...x.recording.ramSpool, enabled: v }))}
                  />
                  {c.recording.ramSpool?.enabled && (
                    <Field label="RAM spool size, MB" hint="When full, calls go to the recordings folder">
                      <NumberInput
                        label="RAM spool size, MB"
                        value={c.recording.ramSpool.sizeMb}
                        onChange={(v) => updateConfig((x) => void (x.recording.ramSpool = { ...x.recording.ramSpool!, sizeMb: Math.max(16, Math.min(8192, Math.round(v || 256))) }))}
                      />
                    </Field>
                  )}
                </>
              )}
            </Group>
            <Group title="Recording" hint="How many calls at once, and how each one is decoded." cols={3}>
              <Field label="Recorders" hint="Calls recorded at once">
                <NumberInput label="Recorders" value={c.recording.maxRecorders} onChange={(v) => updateConfig((x) => void (x.recording.maxRecorders = Math.max(1, Math.min(64, Math.round(v || 32)))))} />
              </Field>
              <Field label="Pre-roll, s" hint="Audio kept from before each grant">
                <NumberInput label="Pre-roll, s" value={c.recording.prerollS} onChange={(v) => updateConfig((x) => void (x.recording.prerollS = Math.max(0, Math.min(3, v || 0))))} />
              </Field>
              <Field label="P25 voice decoder" hint="Fixed-point usually sounds most natural">
                <select value={c.recording.vocoder ?? "fixed"} onChange={(e) => updateConfig((x) => void (x.recording.vocoder = e.target.value as "fixed" | "enhanced" | "mbelib"))}>
                  <option value="fixed">Fixed-point</option>
                  <option value="enhanced">Enhanced</option>
                  <option value="mbelib">mbelib</option>
                </select>
              </Field>
              <Toggle
                label="Save a call heard on several sites once"
                hint="Keeps the cleanest copy, or the Preferred Site's"
                checked={c.recording.dropDuplicateCalls ?? true}
                onChange={(v) => updateConfig((x) => void (x.recording.dropDuplicateCalls = v))}
              />
            </Group>
            {can("service") && (
              <Group title="Startup and diagnostics">
                <Toggle label="Start recording when the app starts" checked={c.server.autoStart} onChange={(v) => updateConfig((x) => void (x.server.autoStart = v))} />
                <Toggle
                  label="Save vocoder frames"
                  hint="As <call>.sdr, for diagnostics and MimoSDR"
                  checked={c.recording.captureFrames}
                  onChange={(v) => updateConfig((x) => void (x.recording.captureFrames = v))}
                />
              </Group>
            )}
          </section>
        </Section>
      )}
      {tab === "recording" && can("keys") && s.lockable.length > 0 && (
        <Section title="Key containers" hint="Encryption keys in a KFDtool container with no password file. Unlocked keys stay in memory until you lock them or the app quits; the password is never saved.">
          {s.lockable.map((k) => (
            <KeyContainer key={k.name} k={k} />
          ))}
        </Section>
      )}
      {tab === "recording" && (
        <Section title="Call rules" hint="Defaults for every system; each system can override them on its card.">
          <CallRules c={c} />
        </Section>
      )}
      {tab === "recording" && can("service") && (
        <Section title="Advanced" hint="Audio encoding and the log.">
          <M4aSettings />
          <LogPanel c={c} />
        </Section>
      )}

      {tab === "plugins" && can("plugins") && <PluginSetupPanel />}
    </div>
  );
}
