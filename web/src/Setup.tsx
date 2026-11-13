import { Fragment, useId, useMemo, useRef, useState } from "react";
import { INDEX_USES } from "./storeIndexes.ts";
import { Fig, Figs, Info, SystemDot, SystemTag } from "./charts.tsx";
import {
  activeSites,
  activeSystems,
  AM_LOW_CUT_MAX_HZ,
  BANDWIDTH_PRESETS,
  defaultBandwidthKhz,
  SQUELCH_HANG_MAX_MS,
  SQUELCH_OPEN_MAX_MS,
  channelsToCsv,
  defaultTalkgroups,
  defaultLog,
  enabledChannels,
  formatFromPath,
  formatMhz,
  mhzCell,
  newConventional,
  newDongle,
  nextTalkgroup,
  parseChannelCsv,
  parseFreqList,
  newSiteName,
  newSystem,
  resolvedCenters,
  runningSource,
  autoTuneOn,
  sameOnTheAir,
  siteActive,
  siteNameOf,
  receiverOf,
  sourceLabels,
  sourceNames,
  systemColor,
  usableHalfWidth,
  siteLockFields,
  siteText,
  systemType,
  deviceKey,
  needsDriver,
  hasBiasT,
  airspyGains,
  pickedBySerial,
  type SerialKind,
  sourceInputs,
  sourceKey,
  foundRadio,
  radioName,
  rateFor,
  rateOk,
  sourceRates,
  ratesText,
  SOURCE_KINDS,
  sourceTitle,
  type Advice,
} from "./config.ts";
import {
  can,
  cardKey,
  currentView,
  dismissTodo,
  downloadText,
  findRadios,
  forgetNames,
  moveNames,
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
  useSelect,
  web,
  type SetupTab,
} from "./controller.ts";
import { M4aSettings, PluginSetupPanel, SystemPluginSettings, useM4aPlugins } from "./Plugins.tsx";
import { renameSystemRefs } from "./pluginSettings.ts";
import { SystemDelays } from "./Delays.tsx";
import { SystemSignals } from "./Signals.tsx";
import { IconAntenna, IconDongle, IconDatabase, IconFolder, IconPuzzle, IconTower } from "./Onboarding.tsx";
import type { AirspyGainMode, Channel, Config, Conventional, Demod, Device, FccLicenses, HeardCode, LockableKeys, LogSettings, Recording, RecordingOverride, RecordingRules, Site, SiteIdentity, SoapyState, Source, StorageSettings, System } from "./protocol.ts";
import { DEFAULT_FORMAT, FilenameEditor } from "./FilenameEditor.tsx";
import { GuardBand } from "./Rolloff.tsx";
import { SurveyPanel } from "./Survey.tsx";
import { FccTable, IdentifyFcc } from "./Identify.tsx";
import { ShareSystem } from "./ShareSystem.tsx";
import { findOnAir, siteFrequencies, type FccTarget } from "./controller.ts";
import { lookForSystem } from "./lookFor.ts";
import { addCard, Card, ExpandAll, Group, Section, setCardOpen, Switch } from "./SetupLayout.tsx";
import { TalkgroupsField, UnitNamesField } from "./TalkgroupsField.tsx";
import { NameFields, ranked, Suggestions, TALKGROUP_NAME_FIELDS, type NameFieldDef } from "./NameFields.tsx";
import { ManageData, RemoveSystem } from "./DataTools.tsx";
import { BiasT, DraftInput, Field, FileInput, NumberInput, ReleaseSlider, TextInput, Toggle } from "./inputs.tsx";
import { asTyped, cleanShortName, lcnTable, mhz, mhzList, msps } from "./fieldText.ts";
import { SourceSuggestion, SourcesAdvice, useSourceAdvice } from "./sourceAdvice.tsx";
import { openTodos } from "./todo.ts";
import { parseAccess, sameAccess, type AccessParts } from "./tones.ts";
import { MODES, type Mode } from "./modes.ts";

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
  return /^(channels|squelch|convplug|convunits|convsignals|conv)-/.test(target) ? "conventional" : "systems";
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
        <h2>Finish the import</h2>
        <Fig v={todos.length} u="left" level="warn" />
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
            <span className="muted small">Until locked or the app quits</span>
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
const modeOptions = (Object.keys(MODES) as Mode[]).map((m) => (
  <option key={m} value={m}>
    {MODES[m].label}
  </option>
));

/** A row's access fields set to `p` (absent when empty). */
function setAccess(x: Channel, p: AccessParts) {
  if (p.code) x.code = p.code;
  else delete x.code;
  if (p.slot !== undefined) x.slot = p.slot;
  else delete x.slot;
  if (p.matchTalkgroup !== undefined) x.matchTalkgroup = p.matchTalkgroup;
  else delete x.matchTalkgroup;
}

/**
 * What a row records: its code (CTCSS / DCS, NAC, colour code, RAN; kept as
 * typed, tidied when left — a pasted `CC1 TS2 TG201` fills the slot and
 * talkgroup too), DMR's slot, and a digital mode's talkgroup.
 */
function AccessInput(props: { ch: Channel; disabled?: boolean; onChange: (p: AccessParts) => void }) {
  const { ch } = props;
  const code = ch.code ?? "";
  const parsed = parseAccess(ch.mode, code);
  const error = "error" in parsed ? parsed.error : null;
  const now: AccessParts = { code, slot: ch.slot, matchTalkgroup: ch.matchTalkgroup };
  const tidy = () => {
    if ("error" in parsed) return;
    const p = { code: parsed.code, slot: parsed.slot ?? ch.slot, matchTalkgroup: parsed.matchTalkgroup ?? ch.matchTalkgroup };
    if (p.code !== code || p.slot !== ch.slot || p.matchTalkgroup !== ch.matchTalkgroup) props.onChange(p);
  };
  return (
    <span className="access-input">
      <DraftInput
        className={`mono narrow${error ? " invalid" : ""}`}
        disabled={props.disabled}
        value={code}
        codec={asTyped}
        placeholder={MODES[ch.mode].code.placeholder}
        readOnly={!MODES[ch.mode].takesCode && !code}
        aria-label="Code"
        aria-invalid={!!error}
        title={error ?? MODES[ch.mode].code.title}
        onChange={(c) => props.onChange({ ...now, code: c })}
        onBlur={tidy}
      />
      {ch.mode === "dmr" && (
        <select
          value={ch.slot ?? ""}
          disabled={props.disabled}
          aria-label="Slot"
          title="The slot it records"
          onChange={(e) => props.onChange({ ...now, slot: e.target.value ? Number(e.target.value) : undefined })}
        >
          <option value="">either slot</option>
          <option value="1">slot 1</option>
          <option value="2">slot 2</option>
        </select>
      )}
      {MODES[ch.mode].matchesTalkgroup && (
        <NumberInput
          narrow
          disabled={props.disabled}
          value={ch.matchTalkgroup}
          placeholder="any TG"
          label="Match TG: only transmissions naming this talkgroup"
          onChange={(v) => props.onChange({ ...now, matchTalkgroup: v === undefined || v <= 0 ? undefined : Math.round(v) })}
        />
      )}
    </span>
  );
}

/** The CSV help's example: the cases a channel list meets. */
const CSV_EXAMPLE = `TG Number,Frequency,Code,Slot,Match TG,Mode,Alpha Tag,Category
1001,154.4300,,,,fm,County Fire Dispatch,Fire
,154.3250,D223N,,,fm,County A Fire,Fire
,154.3250,151.4,,,fm,County B Fire,Fire
46012,460.1250,,,,p25,Townville PD,Police
45351,453.5000,293,,1,p25,Townville PW,Public Works
,453.5000,293,,,p25,Townville (other),Public Works
9001,451.5000,1,1,1,dmr,Dispatch,Business
,119.1000,,,,am,Tower,Airport`;

/** Column, what it holds, what empty means. */
const CSV_COLUMNS: [string, React.ReactNode, string][] = [
  ["Frequency", <>MHz with a decimal point (<code>154.4300</code>), else Hz. Required.</>, "row skipped"],
  ["Mode", <><code>fm</code>, <code>am</code>, <code>p25</code>, <code>dmr</code>, <code>nxdn48</code>, <code>nxdn96</code></>, "fm, or what Code says (a NAC p25, CC dmr, RAN nxdn48)"],
  ["Code", <>The code it records: a tone <code>151.4</code> or DCS <code>D023N</code> (fm), a NAC <code>293</code> (p25), a colour code <code>1</code> (dmr), a RAN <code>5</code> (nxdn)</>, "any"],
  ["Slot", <>dmr: <code>1</code> or <code>2</code></>, "either"],
  ["Match TG", <>p25, dmr, nxdn: only transmissions naming this talkgroup</>, "any"],
  ["TG Number", <>The number its calls are filed under (the archive folder, OpenMHz, the Listen page)</>, "p25 / dmr / nxdn: the air's; else the frequency in kHz (more rows on it: 1543251, 1543252 …)"],
  ["Alpha Tag", "Its name", "the talkgroup file's, by TG"],
  ["Description, Tag, Category", "As in a talkgroup file", "—"],
  ["Squelch dB", "Open threshold, dB above the noise floor, 3–40", "the system's"],
  ["Bandwidth kHz", "fm and am: the channel filter's width, 4–20", "the mode's"],
  ["Enable", <><code>false</code>, <code>no</code>, <code>0</code> or <code>off</code> keeps the row without recording it</>, "on"],
  ["AutoTune", <>p25, dmr, nxdn: <code>true</code> lets its calls correct the source's frequency error (a repeater output to trust)</>, "off"],
];

type MatchKind = "tone" | "code" | "slot" | "tg" | "any";
const KIND_TITLE: Record<MatchKind, string> = {
  tone: "CTCSS tone or DCS code (FM)",
  code: "NAC (P25), colour code (DMR) or RAN (NXDN)",
  slot: "DMR slot",
  tg: "Talkgroup the air names (P25, DMR, NXDN)",
  any: "No code: what no other row on the frequency takes",
};

/** What a row records, as chips: each code it gives, coloured by kind; none: everything (else). */
function matchChips(ch: Channel, shared: boolean): { kind: MatchKind; text: string; bad?: string }[] {
  const code = ch.code ?? "";
  const parsed = parseAccess(ch.mode, code);
  const bad = "error" in parsed ? parsed.error : undefined;
  const out: { kind: MatchKind; text: string; bad?: string }[] = [];
  if (code) {
    const named = MODES[ch.mode].chip(code);
    // (What doesn't read is shown as typed.)
    out.push({ kind: ch.mode === "fm" ? "tone" : "code", text: bad ? code : named, bad });
  }
  if (ch.slot !== undefined) out.push({ kind: "slot", text: `slot ${ch.slot}` });
  if (ch.matchTalkgroup !== undefined) out.push({ kind: "tg", text: `TG ${ch.matchTalkgroup}` });
  if (!out.length) out.push({ kind: "any", text: shared ? "everything else" : "everything" });
  return out;
}

/** A row at a glance: the chips of what it records. Clicking opens the row's editor. */
function MatchLine(props: { ch: Channel; shared: boolean; open: boolean; onToggle: () => void }) {
  const { ch } = props;
  const chips = matchChips(ch, props.shared);
  return (
    <button
      type="button"
      className={`match-line${props.open ? " open" : ""}`}
      aria-expanded={props.open}
      title={!MODES[ch.mode].takesCode ? `${MODES[ch.mode].name} has no codes: every transmission. Click to edit the row` : "What this row records — click to edit the row"}
      onClick={props.onToggle}
    >
      {chips.map((c, k) => (
        <span key={k} className={`match-chip k-${c.kind}${c.bad ? " bad" : ""}`} title={c.bad ?? KIND_TITLE[c.kind]}>
          {c.text}
        </span>
      ))}
    </button>
  );
}

/** The kinds of chip a system's rows can have, for the legend. */
function MatchLegend(props: { modes: Set<Channel["mode"]> }) {
  const m = props.modes;
  const dig = [...m].some((x) => MODES[x].matchesTalkgroup);
  const kinds: [MatchKind, string][] = [
    ...(m.has("fm") ? [["tone", "tone / DCS"] as [MatchKind, string]] : []),
    ...(dig ? [["code", [m.has("p25") && "NAC", m.has("dmr") && "colour code", (m.has("nxdn48") || m.has("nxdn96")) && "RAN"].filter(Boolean).join(" / ")] as [MatchKind, string]] : []),
    ...(m.has("dmr") ? [["slot", "slot"] as [MatchKind, string]] : []),
    ...(dig ? [["tg", "talkgroup"] as [MatchKind, string]] : []),
    ["any", "everything else"],
  ];
  return (
    <div className="match-legend small">
      <span className="muted">Records</span>
      {kinds.map(([k, label]) => (
        <span key={k} className={`match-chip k-${k}`} title={KIND_TITLE[k]}>
          {label}
        </span>
      ))}
      <Info>
        The most specific row that fits takes a transmission. Calls are filed under the row's TG Number{dig ? " (empty: the talkgroup the air names, kept in the call's JSON as talkgroup_ota either way)" : ""}.{" "}
        <b>Edit</b> opens a row's codes and its description, tag and category.
      </Info>
    </div>
  );
}

/** A row's names beside its alpha tag (the table's Name), as a talkgroup file has them. */
type NameField = "description" | "tag" | "group";
/** A row's names beside its name: a talkgroup's (its Category is the row's `group`). */
const NAME_FIELDS: NameFieldDef<NameField>[] = TALKGROUP_NAME_FIELDS.filter((f) => f.key !== "alphaTag").map((f) => ({ ...f, key: f.key === "category" ? "group" : (f.key as NameField) }));

/** The Tag and Category values a conventional system uses already (its rows', its talkgroups'), most used first: offered as each row's are typed. */
function useNameSuggestions(conv: Conventional): { tag: string[]; group: string[] } {
  const file = useApp().talkgroups[conv.shortName];
  return useMemo(() => {
    return {
      tag: ranked([...conv.channels.map((c) => c.tag ?? ""), ...(file ?? []).map((t) => t.tag)]),
      group: ranked([...conv.channels.map((c) => c.group ?? ""), ...(file ?? []).map((t) => t.category)]),
    };
  }, [conv.channels, file]);
}

/**
 * Under a row: what the table has no room for, each labelled. What it
 * records (its code, DMR's slot, a digital mode's talkgroup) and the names
 * its calls carry beside the alpha tag: description, tag and category, as
 * in a talkgroup file (the call JSON's talkgroup_description,
 * talkgroup_group_tag and talkgroup_group; uploads send them on).
 */
function RowEditor(props: {
  ch: Channel;
  disabled: boolean;
  /** The datalists offering the system's tags and categories. */
  lists: Record<"tag" | "group", string>;
  onAccess: (p: AccessParts) => void;
  onName: (field: NameField, value: string) => void;
  onAutoTune: (on: boolean) => void;
  onDone: () => void;
  /** Columns the table has beside the first. */
  span: number;
}) {
  const { ch } = props;
  return (
    <tr className="rule-edit">
      <td />
      <td colSpan={props.span}>
        <div className="row-editor">
          <fieldset>
            <legend>
              Records
              <Info>
                {MODES[ch.mode].code.title}
                {ch.mode === "dmr" && " Slot: either, or one."}
                {MODES[ch.mode].matchesTalkgroup && " Match TG: only transmissions naming this talkgroup (empty: any). Give the row a TG Number to file them under another; the air's stays in the call's JSON as talkgroup_ota."}
              </Info>
            </legend>
            {!MODES[ch.mode].takesCode ? <span className="muted small">Everything ({MODES[ch.mode].name} has no codes)</span> : <AccessInput ch={ch} disabled={props.disabled} onChange={props.onAccess} />}
          </fieldset>
          <fieldset>
            <legend>
              Names
              <Info>Go with every call the row takes, as a talkgroup file's do. Empty: the talkgroup file's for its TG Number, if the row has no names at all.</Info>
            </legend>
            <NameFields fields={NAME_FIELDS} value={(f) => ch[f] ?? ""} lists={props.lists} disabled={props.disabled} onChange={props.onName} />
          </fieldset>
          {MODES[ch.mode].matchesTalkgroup && (
            <fieldset>
              <legend>
                AutoTune
                <Info>
                  Its calls' carrier offsets correct the source's frequency error, when the source has AutoTune on and no control channel. For a repeater output, whose
                  base station holds its frequency; leave it off for simplex or talkaround, where each radio is off by its own amount.
                </Info>
              </legend>
              <label className="toggle">
                <input type="checkbox" checked={!!ch.autoTune} disabled={props.disabled} onChange={(e) => props.onAutoTune(e.target.checked)} />
                <span className="small">Tune the source</span>
              </label>
            </fieldset>
          )}
          <button className="btn small" onClick={props.onDone}>
            Done
          </button>
        </div>
      </td>
    </tr>
  );
}

const SHOWN_CODES = 8;

/**
 * Under a frequency's rows: the codes it has carried (tones, NACs, colour
 * codes), most heard first, each with an Add that makes a row for it — the
 * way to find a frequency's codes without knowing them.
 */
function HeardRow(props: { rows: Channel[]; heard: HeardCode[]; linked: boolean; onAdd: (p: AccessParts) => void; span: number }) {
  const mode = props.rows[0].mode;
  // Only what this mode's Tone can say (NXDN's RAN 0 is any: "RAN 0 TG 201" is "TG 201").
  const nxdn = mode === "nxdn48" || mode === "nxdn96";
  const codes = props.heard.flatMap((h) => {
    const p = parseAccess(mode, h.code);
    return "error" in p || (nxdn && /^RAN 0\b/.test(h.code) && !p.matchTalkgroup) ? [] : [{ ...h, parts: p }];
  });
  if (!codes.length) return null;
  // Already recorded by a row of its own (not just by the catch-all).
  const listed = (p: AccessParts) => props.rows.some((r) => sameAccess({ code: r.code ?? "", slot: r.slot, matchTalkgroup: r.matchTalkgroup }, p));
  const label = (code: string) => (!code ? MODES[mode].none : mode === "fm" && /^\d/.test(code) ? `${code} Hz` : code);
  const shown = codes.slice(0, SHOWN_CODES);
  return (
    <tr className="heard-row">
      <td />
      <td colSpan={props.span}>
        <span className="muted small">Heard:</span>
        {shown.map((h) => (
          <span key={h.code} className="heard-code" title={`Last heard ${new Date(h.lastMs).toLocaleString()}`}>
            <span className="mono">{label(h.code)}</span>
            {h.calls > 0 && <Fig v={h.calls} u={h.calls === 1 ? "call" : "calls"} />}
            {h.skipped > 0 && <Fig v={h.skipped} u="not recorded" level="warn" />}
            {listed(h.parts) ? (
              <span className="muted small" title="Already listed">
                {" "}
                ✓
              </span>
            ) : (
              !props.linked && (
                <button className="btn ghost small" title={`Add a row for ${label(h.code)}`} onClick={() => props.onAdd(h.parts)}>
                  Add
                </button>
              )
            )}
          </span>
        ))}
        {codes.length > SHOWN_CODES && <span className="muted small">+{codes.length - SHOWN_CODES} more</span>}
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
function DriverMissing(props: { kind: Source["type"]; detail: string }) {
  const k = SOURCE_KINDS[props.kind];
  const d = k.driver;
  if (!d) return null;
  return (
    <div className="banner bad small wide driver-missing">
      <span>
        <b>{d.name}</b> not installed
        <Info>
          {k.label} needs it: {props.detail}
        </Info>
      </span>
      <Figs
        items={[
          { k: "macOS", v: <code>{d.macos}</code> },
          { k: "Debian / Ubuntu", v: <code>{d.linux}</code> },
          { k: "Windows", v: d.windows },
          { k: "Then", v: d.then ? `${d.then}, restart` : "Restart the app" },
        ]}
      />
    </div>
  );
}

/** A radio picked by serial from those its driver found (Airspy, HydraSDR, bladeRF, HackRF). */
function SerialPicker(props: { kind: SerialKind; serial: string; used: Set<string>; onChange: (serial: string) => void }) {
  const s = useApp();
  const { kind, serial, used } = props;
  const k = SOURCE_KINDS[kind];
  const state = s.radios?.[kind];
  const devices = state?.devices ?? [];
  return (
    <Field label={k.label} hint={devices.length ? state?.detail : <span className="warn-text">None found</span>} info={devices.length ? undefined : `${state?.detail ?? k.driver?.name}. Plug one in, then Refresh.`}>
      <div className="row">
        <select value={serial} onChange={(e) => props.onChange(e.target.value)}>
          <option value="">First available</option>
          {serial && !devices.some((d) => d.serial === serial) && <option value={serial}>SN {serial} (not connected)</option>}
          {devices.map((d) => {
            const inUse = used.has(deviceKey(kind, d.serial ?? ""));
            return (
              <option key={d.serial} value={d.serial} disabled={inUse}>
                {d.label}
                {inUse ? " (in use)" : ""}
              </option>
            );
          })}
        </select>
        <button className="btn ghost small" disabled={s.findingRadios} onClick={findRadios}>
          Refresh
        </button>
      </div>
    </Field>
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
      <span className="field-label">
        SoapySDR modules
        <Info>
          One per radio family. macOS: <code>brew install soapyhackrf</code>; Debian/Ubuntu: <code>sudo apt install soapysdr0.8-module-all</code>.
        </Info>
      </span>
      {modules === null ? (
        <span className="muted small">
          Not listed by SoapySDR 0.7 <Info>See SoapySDRUtil --info.</Info>
        </span>
      ) : modules.length === 0 ? (
        <span className="warn small">
          None installed
          <Info>SoapySDR can&apos;t open any radio without one.{searchPaths?.length ? ` Looked in ${searchPaths.join(", ")}.` : ""}</Info>
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
    </div>
  );
}

type EditSource = (fn: (x: Source) => void) => void;

function PpmField(props: { src: Exclude<Source, { type: "file" }>; edit: EditSource }) {
  const { src, edit } = props;
  return (
    <Field label="Correction, ppm" info={src.type === "rtlsdr" ? "Whole numbers only: an RTL-SDR's driver takes no fractions." : undefined}>
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

/** USB transfer sizes offered (Trunk Recorder's buflen), bytes; 0 = the driver's 16 KiB. */
const BUF_LENS = [0, 262_144, 131_072, 65_536, 32_768];

/**
 * An RTL-SDR's USB transfer size. The driver's default (16 KiB, 128 in flight) needs no
 * tuning for several dongles on one bus; a larger one is Trunk Recorder's buflen.
 */
function UsbTransfers(props: { src: Extract<Source, { type: "rtlsdr" }>; edit: EditSource }) {
  const { src, edit } = props;
  const len = src.bufLen ?? 0;
  const info = "The default (16 KiB, 128 in flight, about 2 MiB) is right for one dongle or several; larger matches Trunk Recorder's buflen.";
  const kib = (n: number) => `${n / 1024} KiB`;
  return (
    <Field label="USB transfer size" info={info}>
      <select value={len} onChange={(e) => edit((x) => x.type === "rtlsdr" && void (x.bufLen = Number(e.target.value)))}>
        {BUF_LENS.map((n) => (
          <option key={n} value={n}>
            {n === 0 ? "16 KiB (default)" : kib(n)}
          </option>
        ))}
        {!BUF_LENS.includes(len) && <option value={len}>{len % 1024 ? `${len} bytes` : kib(len)}</option>}
      </select>
    </Field>
  );
}

/**
 * AutoTune (Trunk Recorder's): correct the source's channels for the error its
 * control channels show. While recording, what was measured on its radio, and
 * the ppm that would remove it — the recorder's, against the ppm the radio
 * runs with, so setting it twice doesn't move it twice.
 */
function AutoTune(props: { c: Config; src: Source; i: number; edit: EditSource }) {
  const { c, src, i, edit } = props;
  const s = useApp();
  const st = runningSource(s.sources, c, i);
  const err = st?.errorPpm ?? null;
  const apply = src.type !== "file" && err !== null && Math.abs(err) >= 0.3 ? st?.ppmApply ?? null : null;
  return (
    <div className="field wide">
      <Toggle
        label="AutoTune"
        hint="Follows crystal drift"
        info="Measured on its control channels and digital calls."
        checked={!!src.autoTune}
        onChange={(v) =>
          edit((x) => {
            if (v) x.autoTune = true;
            else delete x.autoTune;
          })
        }
      />
      {st && err !== null && (
        <div className="row autotune-figs">
          <Figs
            items={[
              { k: "Measured", v: signedPpm(err), u: "ppm", title: "The frequency error its radio shows" },
              autoTuneOn(st.settings) && st.tunePpm !== undefined && { k: "Corrected", v: signedPpm(st.tunePpm), u: "ppm" },
            ]}
          />
          {apply !== null && src.type !== "file" && apply !== st.settings.ppm &&
            (src.ppm === apply ? (
              <span className="muted small">
                Correction <Fig v={apply} u="ppm" /> from next Start
              </span>
            ) : (
              <button className="btn ghost small" onClick={() => edit((x) => x.type !== "file" && void (x.ppm = apply))}>
                Set correction to {apply} ppm
              </button>
            ))}
        </div>
      )}
    </div>
  );
}

const signedPpm = (x: number) => `${x >= 0 ? "+" : ""}${x.toFixed(2)}`;

const AIRSPY_MODES: { mode: AirspyGainMode; label: string; hint: string }[] = [
  { mode: "linearity", label: "Linearity", hint: "Best near transmitters" },
  { mode: "sensitivity", label: "Sensitivity", hint: "Best for weak signals" },
  { mode: "manual", label: "Each stage", hint: "LNA, mixer and VGA set separately" },
];

/** An Airspy's gain (a HydraSDR's too: the same stages and tables). */
function AirspyGainFields(props: { src: Extract<Source, { type: "airspy" | "hydrasdr" }>; edit: EditSource }) {
  const { src, edit } = props;
  const set = (k: "gainStep" | "lnaStep" | "mixerStep" | "vgaStep", v: number) => edit((x) => airspyGains(x) && void (x[k] = v));
  const slider = (k: "gainStep" | "lnaStep" | "mixerStep" | "vgaStep", label: string, max: number, disabled = false) => (
    <Field label={label}>
      <ReleaseSlider max={max} value={src[k]} disabled={disabled} label={label} text={(v) => (disabled ? "auto" : String(v))} onChange={(v) => set(k, v)} />
    </Field>
  );
  return (
    <>
      <Field label="Gain" hint={AIRSPY_MODES.find((m) => m.mode === src.gainMode)?.hint}>
        <select value={src.gainMode} onChange={(e) => edit((x) => airspyGains(x) && void (x.gainMode = e.target.value as AirspyGainMode))}>
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
          <Toggle label="AGC" hint="LNA and mixer only" checked={src.agc} onChange={(v) => edit((x) => airspyGains(x) && void (x.agc = v))} />
          {slider("lnaStep", "LNA", 14, src.agc)}
          {slider("mixerStep", "Mixer", 15, src.agc)}
          {slider("vgaStep", "VGA (IF)", 15)}
        </>
      )}
    </>
  );
}

/** A HackRF's gain stages: the IF LNA (8 dB steps), the baseband VGA (2 dB steps) and the +14 dB RF amp. */
function HackrfGainFields(props: { src: Extract<Source, { type: "hackrf" }>; edit: EditSource }) {
  const { src, edit } = props;
  const range = (max: number, step: number) => Array.from({ length: max / step + 1 }, (_, n) => n * step);
  const pick = (k: "lnaDb" | "vgaDb", label: string, hint: string | undefined, max: number, step: number) => (
    <Field label={label} hint={hint}>
      <select value={src[k]} aria-label={label} onChange={(e) => edit((x) => x.type === "hackrf" && void (x[k] = Number(e.target.value)))}>
        {!range(max, step).includes(src[k]) && <option value={src[k]}>{src[k]} dB</option>}
        {range(max, step).map((n) => (
          <option key={n} value={n}>
            {n} dB
          </option>
        ))}
      </select>
    </Field>
  );
  return (
    <>
      {pick("lnaDb", "LNA (IF) gain", "Matters most", 40, 8)}
      {pick("vgaDb", "VGA (baseband) gain", undefined, 62, 2)}
      <Toggle label="Amp" hint="+14 dB; overloads near strong signals" checked={src.amp} onChange={(v) => edit((x) => x.type === "hackrf" && void (x.amp = v))} />
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


/** `fresh`: just added (measuring its roll-off is offered). */
/**
 * The sources a system or channel may be received on, as Setup offers them:
 * each radio by its label (what receiveOn names it by, wherever it is in the
 * list), then any it names that isn't a source now, to take off.
 */
function receiveChoices(c: Config, on: string[] | undefined): { label: string; name: string; gone: boolean }[] {
  const named = sourceNames(c);
  const labels = named.map((x) => x.label);
  const here = [...new Set(labels)].map((label) => ({ label, name: `${named.filter((x) => x.label === label).map((x) => x.title).join(", ")} · ${label}`, gone: false }));
  return [...here, ...(on ?? []).filter((l) => !labels.includes(l)).map((label) => ({ label, name: `${label} (not a source now)`, gone: true }))];
}

/** A system's sources: none ticked, any that reaches its channels. Shown with two or more sources. */
function ReceiveOn(props: { c: Config; on: string[] | undefined; onChange: (on: string[] | undefined) => void }) {
  const on = props.on ?? [];
  if (props.c.sources.length < 2 && !on.length) return null;
  return (
    <div className="field wide">
      <span className="field-label">Receive on</span>
      <div className="row">
        {receiveChoices(props.c, on).map((x) => (
          <label key={x.label} className={`toggle${x.gone ? " bad" : ""}`}>
            <input
              type="checkbox"
              checked={on.includes(x.label)}
              onChange={(e) => {
                const next = e.target.checked ? [...on, x.label] : on.filter((l) => l !== x.label);
                props.onChange(next.length ? next : undefined);
              }}
            />
            <span className="small">{x.name}</span>
          </label>
        ))}
      </div>
      <span className="field-hint">{on.length ? "Only those ticked" : "Any source that reaches it"}</span>
    </div>
  );
}

/** A conventional channel's source: any that reaches it, or the one picked (a longer list from a hand-edited config is kept and shown). */
function ReceiveOnSelect(props: { c: Config; ch: Channel; disabled?: boolean; onChange: (on: string[] | undefined) => void }) {
  const on = props.ch.receiveOn ?? [];
  const value = JSON.stringify(on);
  const choices = receiveChoices(props.c, on);
  return (
    <select value={on.length ? value : ""} disabled={props.disabled} aria-label="Source" onChange={(e) => props.onChange(e.target.value ? (JSON.parse(e.target.value) as string[]) : undefined)}>
      <option value="">Any</option>
      {choices.map((x) => (
        <option key={x.label} value={JSON.stringify([x.label])}>
          {x.name}
        </option>
      ))}
      {on.length > 1 && <option value={value}>{on.join(", ")}</option>}
    </select>
  );
}

export function SourceCard(props: { c: Config; i: number; advice?: Advice | null; fresh?: boolean; onFreshDone?: () => void }) {
  const s = useApp();
  const { c, i } = props;
  const src = c.sources[i];
  const center = resolvedCenters(c)[i];
  const auto = src.centerHz ? null : center;
  // The radio picked, as its driver found it: an Airspy R2 runs at 10 / 2.5 MSPS, a Mini at 6 / 3.
  const found = foundRadio(s.radios, src);
  const foundRates = ratesText(found);
  const edit = (fn: (x: Source) => void) => updateConfig((x) => fn(x.sources[i]));
  const setKind = (kind: Source["type"]) =>
    updateConfig((x) => {
      const old = x.sources[i];
      x.sources[i] = { ...SOURCE_KINDS[kind].fresh(), centerHz: old.centerHz } as Source;
    });
  const fileRef = useRef<HTMLInputElement>(null);
  const others = c.sources.filter((_, k) => k !== i);
  const used = new Set(others.map(sourceKey));
  const radios = s.radios;
  // Its driver isn't installed (desktop app).
  const driverState = needsDriver(src) ? radios?.[src.type] : undefined;
  const missing = !!driverState && !driverState.available;
  const needs = useNeed()(`src-${i}`);

  const kindLabel = SOURCE_KINDS[src.type].label;
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
            <h3>{sourceTitle(c, i)}</h3>
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
      <Group title="Device" hint={src.type === "file" ? "A capture, replayed" : undefined}>
        <Field label="Name">
          <TextInput value={src.name ?? ""} placeholder="e.g. the antenna it is on" onChange={(v) => edit((x) => void (x.name = v || undefined))} />
        </Field>
        <div className="field wide">
          <span className="field-label">Kind</span>
          <div className="seg small" role="radiogroup" aria-label={`${sourceTitle(c, i)} kind`}>
            {(Object.keys(SOURCE_KINDS) as Source["type"][])
              .filter((k) => !SOURCE_KINDS[k].driver || can("drivers"))
              .map((k) => (
                <button key={k} role="radio" aria-checked={src.type === k} className={src.type === k ? "on" : ""} onClick={() => setKind(k)}>
                  {SOURCE_KINDS[k].label}
                </button>
              ))}
          </div>
        </div>
        {src.type === "rtlsdr" && (
          <Field label="Dongle" hint={s.devices.length ? undefined : <span className="warn-text">{web ? "None connected" : "None found"}</span>}>
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
        {src.type === "rtlsdr" && can("drivers") && <UsbTransfers src={src} edit={edit} />}
        {missing && <DriverMissing kind={src.type} detail={driverState.detail} />}
        {src.type === "usrp" && !missing && (
            <Field
              label="USRP"
              hint={
                radios?.usrp.devices === null
                  ? radios?.usrp.detail ?? "UHD"
                  : radios?.usrp.devices?.length
                    ? radios.usrp.detail
                    : <span className="warn-text">None found</span>
              }
            >
              <div className="row">
                <TextInput
                  className="mono grow"
                  list={`usrp-${i}`}
                  value={src.args}
                  placeholder="first found, or addr=192.168.10.2"
                  onChange={(v) => edit((x) => x.type === "usrp" && void (x.args = v))}
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
        )}
        {src.type === "usrp" && (
          <Field label="Antenna">
            <TextInput className="mono" value={src.antenna} placeholder="default, e.g. RX2" onChange={(v) => edit((x) => x.type === "usrp" && void (x.antenna = v))} />
          </Field>
        )}
        {src.type === "usrp" && (
          <Field label="Wire format" info="Samples on the USB or network link: sc12 and sc8 take less of it, at some cost in range.">
            <select value={src.wireFormat ?? ""} aria-label="Wire format" onChange={(e) => edit((x) => x.type === "usrp" && void (x.wireFormat = e.target.value || undefined))}>
              <option value="">Auto — sc16, or sc12 for a B2xx on USB 2.0 above 8 MSPS</option>
              <option value="sc16">sc16</option>
              <option value="sc12">sc12 (B2xx)</option>
              <option value="sc8">sc8</option>
            </select>
          </Field>
        )}
        {pickedBySerial(src) && !missing && (
          <SerialPicker
            kind={src.type}
            serial={src.serial}
            used={used}
            onChange={(v) =>
              edit((x) => {
                if (!pickedBySerial(x)) return;
                x.serial = v;
                // Radios of one kind can run at different rates (an Airspy R2 and a Mini): start at one the picked radio does.
                x.rateHz = rateFor(x.rateHz, foundRadio(s.radios, x));
              })
            }
          />
        )}
        {src.type === "hydrasdr" && (
          <Field label="Input" hint="Bias-T is on the first">
            <select value={src.rfPort} aria-label="Input" onChange={(e) => edit((x) => x.type === "hydrasdr" && void (x.rfPort = Number(e.target.value)))}>
              {sourceInputs(src.type).map((p, n) => (
                <option key={p} value={n}>
                  {p}
                </option>
              ))}
            </select>
          </Field>
        )}
        {hasBiasT(src) && <BiasT src={src} checked={src.biasTee} onChange={(v) => edit((x) => hasBiasT(x) && void (x.biasTee = v))} />}
        {airspyGains(src) && (
          <Toggle
            label="Pack samples"
            hint="¼ less USB; off only for old firmware"
            info="12-bit samples on the USB: an Airspy at 10 MSPS nearly fills a USB 2.0 bus without it."
            checked={src.packing ?? true}
            onChange={(v) => edit((x) => airspyGains(x) && void (x.packing = v))}
          />
        )}
        {src.type === "soapy" &&
          (missing ? null : (
            <>
              <Field
                label="Device"
                hint={
                  !radios?.soapy || radios.soapy.devices === null
                    ? radios?.soapy?.detail ?? "SoapySDR"
                    : radios.soapy.devices.length
                      ? radios.soapy.detail
                      : <span className="warn-text">None found</span>
                }
              >
                <div className="row">
                  <TextInput
                    className="mono grow"
                    list={`soapy-${i}`}
                    value={src.args}
                    placeholder="first found, or driver=hackrf"
                    onChange={(v) => edit((x) => x.type === "soapy" && void (x.args = v))}
                  />
                  <datalist id={`soapy-${i}`}>
                    {(radios?.soapy?.devices ?? []).map((d) => (
                      <option key={d.args} value={d.args} disabled={used.has(deviceKey("soapy", d.args))}>
                        {d.label}
                        {used.has(deviceKey("soapy", d.args)) ? " (in use)" : ""}
                      </option>
                    ))}
                  </datalist>
                  <button className="btn ghost small" disabled={s.findingRadios} onClick={findRadios}>
                    {s.findingRadios ? "Searching…" : "Find"}
                  </button>
                </div>
              </Field>
              <Field label="Antenna">
                <TextInput className="mono" value={src.antenna} placeholder="default" onChange={(v) => edit((x) => x.type === "soapy" && void (x.antenna = v))} />
              </Field>
              <Field label="Device settings" info="key=value pairs; SoapySDRUtil --probe lists them." wide>
                <TextInput className="mono" value={src.settings} placeholder="e.g. biastee=true" onChange={(v) => edit((x) => x.type === "soapy" && void (x.settings = v))} />
              </Field>
              {radios?.soapy && <SoapyModules soapy={radios.soapy} />}
            </>
          ))}
        {src.type === "file" &&
          (web ? (
            <Field label="Capture file" hint="rtl_sdr 8-bit IQ; forgotten on reload" wide>
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
              <TextInput
                className="mono"
                value={src.path}
                placeholder="/path/to/capture.cu8"
                onChange={(v) =>
                  edit((x) => {
                    if (x.type !== "file") return;
                    x.path = v;
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
      <Group title="Tuning" hint="What falls in the band is recorded">
        <Field
          label="Center frequency, MHz"
          hint={src.centerHz ? undefined : auto ? <>Auto <Fig v={formatMhz(auto, 4)} u="MHz" /></> : "Auto: nothing left to cover"}
        >
          <DraftInput className="mono" codec={mhz} value={src.centerHz} placeholder={auto ? formatMhz(auto, 4) : "MHz"} onChange={(hz) => edit((x) => void (x.centerHz = hz))} />
        </Field>
        <Field
          label="Sample rate"
          hint={
            foundRates && !rateOk(src.rateHz, found) ? (
              <span className="warn-text" title={`This ${radioName(src.type, found)} runs at ${foundRates} MSPS, not ${src.rateHz / 1e6}`}>
                Won&apos;t start · {radioName(src.type, found)}: {foundRates}
              </span>
            ) : foundRates ? (
              `This ${radioName(src.type, found)}: ${foundRates}`
            ) : (
              SOURCE_KINDS[src.type].rateHint
            )
          }
        >
          <RateInput
            key={src.type}
            hz={src.rateHz}
            free={!!SOURCE_KINDS[src.type].freeRate || (src.type === "file" && can("service"))}
            options={sourceRates(src.type, found)}
            onChange={(hz) => edit((x) => void (x.rateHz = hz))}
          />
        </Field>
        {src.type === "bladerf" && (
          <Field label="Filter bandwidth, MHz" info="The analog low-pass filter. Blank: the sample rate.">
            <DraftInput className="mono" codec={mhz} value={src.bandwidthHz} placeholder={formatMhz(src.rateHz, 2)} aria-label="Filter bandwidth, MHz" onChange={(hz) => edit((x) => x.type === "bladerf" && void (x.bandwidthHz = hz))} />
          </Field>
        )}
        <div className="wide">
          <SourceSuggestion c={c} i={i} advice={props.advice ?? null} />
        </div>
        <GuardBand c={c} i={i} fresh={props.fresh} onFreshDone={props.onFreshDone} />
        {center ? (
          <div className="wide">
            <CoverageBar c={c} center={center} rateHz={src.rateHz} guardHz={src.guardHz} />
          </div>
        ) : null}
      </Group>
      {src.type !== "file" && (
        <Group
          title="Gain"
          hint={
            <>
              {SOURCE_KINDS[src.type].liveAgc || src.type === "hackrf" ? "Live, also while recording" : "dB live; AGC from next Start"}
              <Info>
                {airspyGains(src) ? "A preset, or each stage set by hand. " : ""}Too little loses weak signals; too much overloads near strong ones.
                {SOURCE_KINDS[src.type].liveAgc || src.type === "hackrf" ? "" : " The gain in dB applies at once, also while recording; AGC on or off when recording starts."}
              </Info>
            </>
          }
        >
          {src.type === "rtlsdr" && (
            <Field label="Gain, dB" hint={src.agc ? "Fixed usually works better" : "0–49.6"} info="Most dongles: 0–49.6 in steps (0.9, 1.4, 2.7 … 49.6); the nearest is used.">
              <div className="row">
                <NumberInput gain label="Gain, dB" value={src.gainDb} disabled={src.agc} onChange={(v) => v !== undefined && edit((x) => x.type === "rtlsdr" && void (x.gainDb = v))} />
                <Toggle label="AGC" checked={src.agc} onChange={(v) => edit((x) => x.type === "rtlsdr" && void (x.agc = v))} />
              </div>
            </Field>
          )}
          {src.type === "usrp" && (
            <Field label="Gain, dB" hint={src.agc ? "AGC: B2xx and E3xx only" : "B200/B210: 0–76"}>
              <div className="row">
                <NumberInput gain label="Gain, dB" value={src.gainDb} disabled={src.agc} onChange={(v) => edit((x) => x.type === "usrp" && void (x.gainDb = v || 0))} />
                <Toggle label="AGC" checked={src.agc} onChange={(v) => edit((x) => x.type === "usrp" && void (x.agc = v))} />
              </div>
            </Field>
          )}
          {src.type === "bladerf" && (
            <Field label="Gain, dB" hint={src.agc ? "Fixed usually works better" : "bladeRF 2.0: about −15 to 60"}>
              <div className="row">
                <NumberInput gain label="Gain, dB" value={src.gainDb} disabled={src.agc} onChange={(v) => edit((x) => x.type === "bladerf" && void (x.gainDb = v ?? 0))} />
                <Toggle label="AGC" checked={src.agc} onChange={(v) => edit((x) => x.type === "bladerf" && void (x.agc = v))} />
              </div>
            </Field>
          )}
          {airspyGains(src) && <AirspyGainFields src={src} edit={edit} />}
          {src.type === "hackrf" && <HackrfGainFields src={src} edit={edit} />}
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
      <Group title="Frequency correction" hint={<>Crystal error<Info>Its crystal&apos;s error, corrected so channels land where they should.</Info></>}>
        {src.type !== "file" && <PpmField src={src} edit={edit} />}
        <AutoTune c={c} src={src} i={i} edit={edit} />
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
          <SystemTag key={x.shortName} config={c} name={x.shortName} />
        ))}
        {conv.length > 0 && <Fig v={conv.length} u="conventional" />}
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
      hint="FM, AM, P25, DMR and NXDN channels"
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
export function ConventionalPanel(props: { c: Config; k: number }) {
  const { c, k } = props;
  const need = useNeed();
  const conv = c.conventional[k];
  const chans = conv.channels;
  const multi = c.sources.length > 1;
  const name = conv.shortName;
  const dupName = c.conventional.some((x, j) => j !== k && x.shortName === name);
  // Stable row keys (each edit clones the config; a frequency input keeps its own text).
  const ids = useRef<number[]>([]);
  if (ids.current.length !== chans.length) ids.current = chans.map(() => nextRowId++);
  const csvRef = useRef<HTMLInputElement>(null);
  const [bulk, setBulk] = useState("");
  const [pending, setPending] = useState<{ name: string; channels: Channel[]; notes: string[] } | null>(null);
  // (Asked in a dialog: whether its data in the store goes too.)
  const [removing, setRemoving] = useState(false);
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
  const suggest = useNameSuggestions(conv);
  const listId = useId();
  const lists = { tag: `${listId}-tag`, group: `${listId}-group` };
  /** The rows (by id) whose editor is open. */
  const [editing, setEditing] = useState<Set<number>>(() => new Set());
  const toggleEdit = (id: number) =>
    setEditing((e) => {
      const n = new Set(e);
      if (!n.delete(id)) n.add(id);
      return n;
    });
  /** The last row on row i's frequency. */
  const lastOfFreq = (i: number) => chans.reduce((at, o, k) => (Math.abs(o.freqHz - chans[i].freqHz) < 1 ? Math.max(at, k) : at), i);
  /** Another row on row i's frequency (after the last of them), for another code — `access` when it is known. */
  const addRule = (i: number, access: AccessParts = { code: "" }) => {
    const ch = chans[i];
    const at = lastOfFreq(i);
    const row: Channel = { freqHz: ch.freqHz, mode: ch.mode, name: "", enabled: true };
    setAccess(row, access);
    if (ch.bandwidthKhz !== undefined) row.bandwidthKhz = ch.bandwidthKhz;
    // Analog: the next free number. Digital: the air's, until one is typed.
    if (!MODES[ch.mode].airNamesTalkgroup) row.talkgroup = nextTalkgroup(chans, ch.freqHz);
    if (ch.squelchDb !== undefined) row.squelchDb = ch.squelchDb;
    const id = nextRowId++;
    ids.current.splice(at + 1, 0, id);
    // A new rule's editor opens, unless it came with its code.
    if (!access.code && access.matchTalkgroup === undefined) setEditing((e) => new Set(e).add(id));
    edit((x) => void x.channels.splice(at + 1, 0, row));
  };
  const talkgroups = defaultTalkgroups(chans);
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
              <Fig v={chans.length} u={chans.length === 1 ? "channel" : "channels"} />
              {chans.length > 0 && (
                <>
                  {" · "}
                  <Fig v={enabled} u="on" />
                </>
              )}
            </span>
          </div>
          {todo > 0 && <span className="chip warn">{todo} to finish</span>}
        </>
      }
      actions={
        <>
          <Switch label="Record" checked={conv.enabled} onChange={(v) => edit((x) => void (x.enabled = v))} />
          <IdentifyFcc target={{ kind: "conventional", index: k }} what={conv.name || conv.shortName} freqs={[...new Set(chans.map((ch) => ch.freqHz).filter((f) => f > 0))]} fcc={conv.fcc} />
          <button className="btn ghost small" onClick={() => setRemoving(true)}>
            Remove
          </button>
          {removing && (
            <RemoveSystem
              name={name || conv.shortName || "this conventional system"}
              id={conv.id}
              question={`Its ${chans.length} channel${chans.length === 1 ? " goes" : "s go"} with it; its recordings stay on disk.`}
              onRemove={() =>
                updateConfig((x) => {
                  x.conventional.splice(k, 1);
                  forgetNames(x, conv.shortName);
                })
              }
              onClose={() => setRemoving(false)}
            />
          )}
          <FileInput inputRef={csvRef} accept=".csv,text/csv" onFile={(f) => void onCsv(f)} />
        </>
      }
    >
      <Group title="Basics" cols={3}>
        <Field label="Short name" hint={dupName ? "Already used — each needs its own folder" : "Its calls' folder"}>
          <TextInput
            value={conv.shortName}
            onChange={(v) => {
              const to = cleanShortName(v);
              updateConfig((x) => renameSystem(x, x.conventional[k], to));
            }}
          />
        </Field>
        <Field label="Name">
          <TextInput value={conv.name ?? ""} placeholder="County Fire" onChange={(v) => edit((x) => void (v ? (x.name = v) : delete x.name))} />
        </Field>
        <Field needs={need(`squelch-${name}`)} anchor={`squelch-${name}`} label="Squelch, dB" hint={<>Channels' default <Info>dB above the noise floor, 3–40. A channel's own Squelch wins.</Info></>}>
          <NumberInput label="Squelch, dB above noise" value={conv.squelchDb} onChange={(v) => edit((x) => void (x.squelchDb = Math.max(3, Math.min(40, v ?? 8))))} />
        </Field>
      </Group>
      <FccLicensesGroup
        target={{ kind: "conventional", index: k }}
        fcc={conv.fcc}
        foldKey={`fcc:conv:${name}`}
        hint={<>Who holds each frequency <Info>Who is licensed on each channel's frequency (they can be different licensees'). Identify looks them up; Motorola P25 channels say theirs on the air.</Info></>}
      />
      {chans.some((c) => !MODES[c.mode].analog) && (
        <Group title="Talkgroup names" hint={<>Digital channels' <Info>For the talkgroups its P25, DMR and NXDN channels name on the air. A row's own name is for calls filed under its TG Number.</Info></>}>
          <TalkgroupsField sys={conv} />
        </Group>
      )}
      <Group
        title="Channels"
        hint={<>Rows can share a frequency <Info>Each row: a frequency, what it records (tone, NAC, colour code, RAN, slot, talkgroup), the TG Number its calls are filed under, and their names.</Info></>}
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
              <Fig v={pending.channels.length} u={pending.channels.length === 1 ? "channel" : "channels"} /> in <b>{pending.name}</b>
              {pending.notes.length > 0 && <div className="small muted">{pending.notes.join(" ")}</div>}
            </div>
            <div className="row">
              <button
                className="btn primary"
                onClick={() => {
                  replaceAll(pending.channels);
                  setPending(null);
                }}
              >
                Replace {chans.length}
              </button>
              <button
                className="btn"
                onClick={() => {
                  add(pending.channels);
                  setPending(null);
                }}
              >
                Add to them
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
              From <code>{conv.channelFile}</code> <Info>Re-read each time recording starts</Info>
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
          <Field label="Add frequencies, MHz" hint="Commas or spaces between">
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
            <MatchLegend modes={new Set(chans.map((c) => c.mode))} />
            {(["tag", "group"] as const).map((f) => (
              <Suggestions key={f} id={lists[f]} values={suggest[f]} />
            ))}
            <div className="table-wrap channels-wrap">
              <table className="calls channels">
                <thead>
                  <tr>
                    <th title="Record this channel">On</th>
                    <th>Frequency, MHz</th>
                    <th>Mode</th>
                    <th title="Analog: the channel filter's width">Bandwidth</th>
                    <th title="What the row records: a tone, NAC, colour code or RAN; DMR's slot; a digital mode's talkgroup. Click a row's to change it">Records</th>
                    <th>Name</th>
                    <th title="Calls are filed under it. Empty: digital modes the talkgroup the air names, else the frequency in kHz. The air's is kept in the call's JSON as talkgroup_ota">TG Number</th>
                    <th title="dB above the noise floor">Squelch</th>
                    {multi && <th title="Which source receives it: any that covers it, or just the one picked">Source</th>}
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
                              // The code is the mode's; Match TG only digital modes', Slot only DMR's.
                              if (!MODES[x.mode].takesCode) delete x.code;
                              if (!MODES[x.mode].matchesTalkgroup) delete x.matchTalkgroup;
                              if (x.mode !== "dmr") delete x.slot;
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
                        <MatchLine
                          ch={ch}
                          shared={shared(i)}
                          open={editing.has(ids.current[i])}
                          onToggle={() => toggleEdit(ids.current[i])}
                        />
                      </td>
                      <td>
                        <TextInput value={ch.name} disabled={linked} placeholder="Name" aria-label="Name (alpha tag)" onChange={(v) => editRow(i, (x) => void (x.name = v))} />
                        {(ch.tag || ch.group || ch.description) && (
                          <button type="button" className="row-meta small" title={ch.description || "Its description, tag and category — click to edit"} onClick={() => toggleEdit(ids.current[i])}>
                            {[ch.tag, ch.group].filter(Boolean).join(" · ") || ch.description}
                          </button>
                        )}
                        {ch.autoTune && MODES[ch.mode].matchesTalkgroup && (
                          <span className="row-meta small muted" title="Its calls correct the source's frequency error">
                            AutoTune
                          </span>
                        )}
                      </td>
                      <td>
                        <NumberInput
                          narrow
                          disabled={linked}
                          value={ch.talkgroup}
                          placeholder={ch.freqHz ? (MODES[ch.mode].airNamesTalkgroup ? "air's" : String(talkgroups[i])) : "auto"}
                          label={
                            MODES[ch.mode].airNamesTalkgroup
                              ? `TG Number: calls are filed under it; empty: the talkgroup the air names (${ch.freqHz ? talkgroups[i] : "the frequency in kHz"} when it names none)`
                              : "TG Number: calls are filed under it"
                          }
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
                      {multi && (
                        <td>
                          <ReceiveOnSelect
                            c={c}
                            ch={ch}
                            onChange={(on) =>
                              edit((x) => {
                                // Rows sharing the frequency share them.
                                for (const r of x.channels.filter((o) => Math.abs(o.freqHz - ch.freqHz) < 1)) {
                                  if (on === undefined) delete r.receiveOn;
                                  else r.receiveOn = on;
                                }
                              })
                            }
                          />
                        </td>
                      )}
                      <td>
                        <span className="row-actions">
                          <button
                            className="btn ghost small"
                            aria-expanded={editing.has(ids.current[i])}
                            title="Its codes, and its description, tag and category"
                            onClick={() => toggleEdit(ids.current[i])}
                          >
                            Edit
                          </button>
                        {!linked && (
                          <>
                            {ch.freqHz > 0 && MODES[ch.mode].add && (
                              <button
                                className="btn ghost small"
                                title={`Another ${MODES[ch.mode].add?.by} on this frequency`}
                                aria-label="Add a row on this frequency"
                                onClick={() => addRule(i)}
                              >
                                {MODES[ch.mode].add?.button}
                              </button>
                            )}
                            <button className="btn ghost small danger" title="Remove" aria-label="Remove channel" onClick={() => remove(i)}>
                              ×
                            </button>
                          </>
                        )}
                        </span>
                        </td>
                      </tr>
                      {(editing.has(ids.current[i]) || "error" in parseAccess(ch.mode, ch.code ?? "")) && (
                        <RowEditor
                          key={`e${ch.mode}`}
                          ch={ch}
                          disabled={linked}
                          lists={lists}
                          onAccess={(p) => editRow(i, (x) => setAccess(x, p))}
                          onName={(f, v) => editRow(i, (x) => void (v ? (x[f] = v) : delete x[f]))}
                          onAutoTune={(on) => editRow(i, (x) => void (on ? (x.autoTune = true) : delete x.autoTune))}
                          onDone={() => setEditing((e) => (e.delete(ids.current[i]) ? new Set(e) : e))}
                          span={multi ? 9 : 8}
                        />
                      )}
                      {lastOfFreq(i) === i && heard[String(Math.round(ch.freqHz))] && (
                        <HeardRow
                          rows={chans.filter((o) => Math.abs(o.freqHz - ch.freqHz) < 1)}
                          heard={heard[String(Math.round(ch.freqHz))]}
                          linked={linked}
                          onAdd={(p) => addRule(i, p)}
                          span={multi ? 9 : 8}
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
            label="Channel file"
            hint={<>Optional: edit in a spreadsheet <Info>A CSV, re-read at each start; created from this list if new</Info></>}
          >
            <div className="row">
              <input className="mono grow" value={filePath} placeholder="channels.csv" onChange={(e) => setFilePath(e.target.value)} />
              <button className="btn" disabled={!filePath.trim()} onClick={() => setChannelFile(k, filePath.trim())}>
                Use this file
              </button>
            </div>
          </Field>
        )}
        <details className="help csv-help">
          <summary>CSV format</summary>
          <p className="small">
            A row per channel, under a header row (any order, any case; other columns ignored). Start from <b>Export CSV</b>; Trunk Recorder's channel
            files read as they are.
          </p>
          <pre className="mono small csv-example">{CSV_EXAMPLE}</pre>
          <table className="csv-columns small">
            <thead>
              <tr>
                <th>Column</th>
                <th>What it holds</th>
                <th>Empty</th>
              </tr>
            </thead>
            <tbody>
              {CSV_COLUMNS.map(([col, what, empty]) => (
                <tr key={col}>
                  <td>
                    <code>{col}</code>
                  </td>
                  <td>{what}</td>
                  <td className="muted">{empty}</td>
                </tr>
              ))}
            </tbody>
          </table>
          <ul className="small">
            <li>
              Rows with the same <code>Frequency</code> split it: a transmission goes to the most specific row it fits (one row may give no{" "}
              <code>Code</code>, <code>Slot</code> or <code>Match TG</code>: it takes the rest), else it isn't recorded.
            </li>
            <li>
              <code>Code</code> takes RadioReference's spellings — <code>151.4 PL</code>, <code>023 DPL</code>, <code>293 NAC</code>, <code>CC1 TS2 TG201</code>,{" "}
              <code>RAN 5 TG 201</code> — and a slot or talkgroup in it fills <code>Slot</code> and <code>Match TG</code>.
            </li>
            <li>
              A P25, DMR or NXDN call keeps the talkgroup the air named in its JSON (<code>talkgroup_ota</code>), also when <code>TG Number</code> files it
              under another.
            </li>
            <li>
              Commas, semicolons or tabs; quoted fields; decimal commas (<code>154,43</code>) with semicolons, as Excel writes them in some locales. Blank
              rows and <code>#</code> comments are skipped. A value that can't be read is left empty, and the row is named after reading.
            </li>
            <li>
              From Trunk Recorder: its <code>Tone</code> column reads as <code>Code</code>, and its <code>Squelch</code> (an absolute level) isn't used.
            </li>
          </ul>
        </details>
      </Group>
      {chans.some((ch) => MODES[ch.mode].analogReceiver) && (
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
        <Group title="Units" cols={1}>
          <UnitNamesField
            system={conv.shortName}
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
        <Group title="Plugins" hint="Default: the Plugins tab's" cols={1}>
          <SystemPluginSettings system={{ conv: k }} />
          <SystemDelays system={{ conv: k }} />
          <SystemSignals system={{ conv: k }} />
        </Group>
      )}
      {chans.length > 0 && (
        <Group title="Recording" hint="Default: the Recording tab's" cols={1}>
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
  const fm = props.conv.channels.some((ch) => MODES[ch.mode].hasFm);
  const am = props.conv.channels.some((ch) => ch.mode === "am");
  const set = <K extends keyof Demod>(k: K, v: Demod[K] | undefined) =>
    props.edit((x) => {
      if (v === undefined) delete x[k];
      else x[k] = v;
    });
  const clamp = (v: number | undefined, max: number) => (v === undefined ? undefined : Math.max(0, Math.min(max, Math.round(v))));
  return (
    <Group title="Demodulation" hint={<>Analog audio <Info>How the analog channels become audio. Default suits most channels; each channel's bandwidth is in its row.</Info></>} cols={3}>
      {fm && (
        <Field label="FM de-emphasis" hint={<>Off if audio is muffled <Info>For channels sent without pre-emphasis: they sound muffled with it</Info></>}>
          <select value={d.fmDeemphasis === false ? "off" : ""} onChange={(e) => set("fmDeemphasis", e.target.value === "off" ? false : undefined)}>
            <option value="">Default (750 µs)</option>
            <option value="off">Off</option>
          </select>
        </Field>
      )}
      {am && (
        <Field label="AM level" hint={<>Follows a fading carrier <Info>How fast the volume follows a fading carrier: Fast keeps an aircraft steady, Slow pumps less on a steady signal</Info></>}>
          <select value={d.amLevel ?? ""} onChange={(e) => set("amLevel", e.target.value === "slow" ? "slow" : undefined)}>
            <option value="">Fast, 50 ms (default): aircraft</option>
            <option value="slow">Slow, 0.5 s: steady signals</option>
          </select>
        </Field>
      )}
      {am && (
        <Field label="AM low cut, Hz" hint="Cuts hum and flutter · 0: off">
          <NumberInput label="AM low cut, Hz" value={d.amLowCutHz} placeholder="200" onChange={(v) => set("amLowCutHz", clamp(v, AM_LOW_CUT_MAX_HZ))} />
        </Field>
      )}
      <Field label="Squelch open delay, ms" hint={<>Ignores splatter <Info>The carrier must hold this long, so a neighbour's splatter doesn't open it</Info></>}>
        <NumberInput label="Squelch open delay, ms" value={d.squelchOpenMs} placeholder="30" onChange={(v) => set("squelchOpenMs", clamp(v, SQUELCH_OPEN_MAX_MS))} />
      </Field>
      <Field label="Squelch hang, ms" hint={<>Bridges fades <Info>Bridges fades on weak signals; the noise after a transmission is still cut. Live audio runs this much later.</Info></>}>
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
      <LcnTableField site={site} edit={edit} label="Channel table" hint="Optional · channel=MHz: 101=452.275" placeholder="learned from the air" />
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
      <Field label={typeD ? "More repeaters, MHz" : "Voice frequencies, MHz"} hint={typeD ? "Optional: or list all above" : "Teaches channel numbers"} wide>
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
        label="Channel table"
        hint={
          typeD ? (
            "Optional · repeater=MHz: 1=451.0125"
          ) : (
            <>
              Optional · channel=MHz: 20=451.0125{" "}
              <Info>
                A Type-C control channel grants channel numbers. Their frequencies come from this table (RadioReference lists it for many systems), from the site if it uses
                Direct Frequency Assignment, or are learned when the voice frequencies are listed.
              </Info>
            </>
          )
        }
        placeholder={typeD ? "none" : "learned from the air"}
      />
    </>
  );
}

/** A site's `lcnTableHz`: channel number = MHz (DMR, NXDN, EDACS). */
function LcnTableField(props: { site: Site; edit: (fn: (x: Site) => void) => void; label: string; hint: React.ReactNode; placeholder: string }) {
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
      <LcnTableField
        site={site}
        edit={edit}
        label="Channels (LCNs)"
        hint={
          <>
            LCN=MHz: 1=856.1875, 3=857.7125{" "}
            <Info>
              The control channel grants calls a channel number (LCN); this list (RadioReference has it) turns it into a frequency. Every listed frequency is watched, so a
              list with its numbers mixed up is put right from the air. A call on an LCN whose frequency isn't listed is logged and not recorded.
            </Info>
          </>
        }
        placeholder="1=856.1875, 2=…"
      />
      <Toggle
        label="ESK 0xA0"
        hint="Scrambled control channel (some Harris)"
        checked={!!site.esk}
        onChange={(v) =>
          edit((x) => {
            if (v) x.esk = true;
            else delete x.esk;
          })
        }
      />
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
          <Field label="Base, Hz" hint="The offset channel's">
            <NumberInput label="Band plan base" placeholder="489087500" value={site.bandplanBaseHz} onChange={(v) => setNum("bandplanBaseHz", v)} />
          </Field>
          <Field label="Spacing, Hz">
            <NumberInput label="Band plan spacing" placeholder="25000" value={site.bandplanSpacingHz} onChange={(v) => setNum("bandplanSpacingHz", v)} />
          </Field>
          <Field label="Offset (channel)">
            <NumberInput label="Band plan offset" placeholder="380" value={site.bandplanOffset} onChange={(v) => setNum("bandplanOffset", v)} />
          </Field>
          <Field label="High, Hz" hint="Top outbound channel">
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
  ["signals", "signals"],
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
  const ccOn = [...new Set(ccs.map((f) => receiverOf(c, sys.shortName, f)).filter((k) => k >= 0))];
  const voiceIn = voice.filter((f) => receiverOf(c, sys.shortName, f) >= 0).length;
  return (
    <>
      {ccs.length > 0 &&
        (ccOn.length ? (
          <span className="chip ok" title={`Control channel on source ${ccOn.map((k) => k + 1).join(", ")}`}>
            CC → {ccOn.map((k) => k + 1).join(", ")}
          </span>
        ) : (
          <span className="chip bad" title="No control channel inside a source">
            CC → none
          </span>
        ))}
      {voice.length > 0 && (
        <span className={`chip ${voiceIn === voice.length ? "ok" : "warn"}`} title={`Voice channels found by the survey: ${voiceIn} of ${voice.length} inside a source`}>
          Voice {voiceIn}/{voice.length}
        </span>
      )}
    </>
  );
}

/** One site of system `i`: folded, its name, Record switch, control channels and coverage; open, how it is followed. */
/** A multi-site system's preferred site: the one whose copy of a call is kept for talkgroups whose row names none. */
function PreferredSite(props: { sys: System; edit: (fn: (x: System) => void) => void }) {
  const { sys, edit } = props;
  const names = sys.sites.map((x) => siteNameOf(sys, x));
  const value = names.find((n) => n.toLowerCase() === sys.preferredSite?.toLowerCase()) ?? "";
  return (
    <Field label="Preferred site" hint={<>Its copy is kept <Info>Its copy of a call is kept unless another site's is clearly cleaner. A talkgroup's Preferred Site comes first.</Info></>}>
      <select value={value} onChange={(e) => edit((x) => void (e.target.value ? (x.preferredSite = e.target.value) : delete x.preferredSite))}>
        <option value="">None: cleanest copy</option>
        {names.map((n) => (
          <option key={n} value={n}>
            {n}
          </option>
        ))}
      </select>
    </Field>
  );
}

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
    updateConfig((x) => {
      const s = x.systems[i];
      if (s.preferredSite?.toLowerCase() === name.toLowerCase()) delete s.preferredSite;
      s.sites.splice(k, 1);
    });
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
        <Field label="Site name" hint={dupName ? "Another site has it — each needs its own" : "Talkgroups' Preferred Site"}>
          <TextInput
            value={site.name ?? ""}
            placeholder={sys.shortName}
            onChange={(v) =>
              updateConfig((c) => {
                const s = c.systems[i];
                const x = s.sites[k];
                const was = siteNameOf(s, x);
                if (v) x.name = v;
                else delete x.name;
                // The system's preferred site follows its name.
                if (s.preferredSite?.toLowerCase() === was.toLowerCase()) s.preferredSite = siteNameOf(s, x);
              })
            }
          />
        </Field>
        {sys.type !== "dmr" && sys.type !== "nxdn" && sys.type !== "edacs" && (
          <Field label={sys.type === "smartnet" ? "P25 voice modulation" : "Modulation"}>
            <select value={site.modulation} onChange={(e) => edit((x) => void (x.modulation = e.target.value as Site["modulation"]))}>
              <option value="auto">Detect</option>
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
              ? "Capacity Plus: every repeater; else control channels"
              : typeD
                ? "Every repeater of the site"
                : sys.type === "edacs"
                  ? "List more (or all) if it moves"
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
              Site lock {locked ? <span className="chip ok">{siteText(lockedTo)}</span> : <span className="muted">off</span>}
            </summary>
            <p className="muted small">
              {need(`site-${at}`) && (
                <>
                  <span className="field-needs">{need(`site-${at}`)}</span>{" "}
                </>
              )}
              This site only, not a neighbour · empty: any{" "}
              <Info>
                Follow only a control channel announcing this site.{" "}
                {sys.type === "edacs" ? "The site number as the control channel says it." : sys.type === "smartnet" || sys.type === "nxdn" ? "System ID in hex." : "NAC, WACN and System ID in hex."}
              </Info>
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

/** A system's or a conventional system's FCC licenses: only once a frequency has one (Identify, the air, or the config fills it). */
/** Licenses open at first up to this many; more fold to a line (a statewide system can have dozens). */
const FCC_OPEN_UP_TO = 6;

function FccLicensesGroup(props: { target: FccTarget; fcc: FccLicenses | undefined; hint: React.ReactNode; foldKey: string }) {
  const rows = Object.values(props.fcc ?? {});
  if (!rows.length) return null;
  const calls = [...new Set(rows.map((l) => l.callSign).filter(Boolean))];
  // (Inline figures: the summary is a paragraph.)
  const summary = (
    <>
      <Fig v={rows.length} u={rows.length === 1 ? "frequency" : "frequencies"} />
      {calls.length > 0 && (
        <>
          {" · "}
          <Fig v={calls.length} u={calls.length === 1 ? "call sign" : "call signs"} /> <span className="mono">{`${calls.slice(0, 4).join(", ")}${calls.length > 4 ? "…" : ""}`}</span>
        </>
      )}
    </>
  );
  return (
    <Group title="FCC licenses" hint={props.hint} cols={1} fold={{ key: props.foldKey, open: rows.length <= FCC_OPEN_UP_TO, summary }}>
      <FccTable target={props.target} fcc={props.fcc} />
    </Group>
  );
}

/** Which of a system's sites can be heard here: Find my system, looking for it. */
function checkSites(sys: System): void {
  const why = findOnAir(lookForSystem(sys));
  if (why) setNotice(`${why} Find my system, below, is looking for ${sys.shortName}'s sites when you scan.`);
}

export function SystemCard(props: { c: Config; i: number }) {
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

  // (Asked in a dialog: whether its data in the store goes too. `true`: removing its only site.)
  const [removing, setRemoving] = useState<boolean | null>(null);
  const remove = (lastSite = false) => setRemoving(lastSite);
  const removed = () =>
    updateConfig((x) => {
      x.systems.splice(i, 1);
      forgetNames(x, sys.shortName);
    });
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
    // Its talkgroups come along if this one has none.
    const [theirs, mine] = [app.talkgroups[other.shortName], app.talkgroups[sys.shortName]];
    if (theirs && mine && JSON.stringify(theirs) !== JSON.stringify(mine)) dropped.push("talkgroups");
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
      if (!mine) moveNames(from.shortName, to.shortName);
      x.systems.splice(j, 1);
      forgetNames(x, from.shortName);
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
          <SystemDot color={color} />
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
          {lookForSystem(sys).sites.length > 0 && (
            <button className="btn ghost small" onClick={() => checkSites(sys)} title="Which sites are heard here: the survey listens to each one's control channels">
              Check sites
            </button>
          )}
          <button className="btn ghost small" onClick={() => remove()}>
            Remove
          </button>
          {removing !== null && (
            <RemoveSystem name={sys.shortName} id={sys.id} question={`${removing ? `This is ${sys.shortName}'s only site. ` : ""}Its recordings stay on disk.`} onRemove={removed} onClose={() => setRemoving(null)} />
          )}
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
      <Group title="Basics">
        <Field label="Short name" hint={dupName ? "Already used — each needs its own folder" : "Its calls' folder"}>
          <TextInput
            value={sys.shortName}
            onChange={(v) => {
              const to = cleanShortName(v);
              updateConfig((x) => renameSystem(x, x.systems[i], to));
            }}
          />
        </Field>
        <Field label="Name">
          <TextInput value={sys.name ?? ""} placeholder="County Public Safety" onChange={(v) => edit((x) => void (x.name = v))} />
        </Field>
        <ReceiveOn c={props.c} on={sys.receiveOn} onChange={(on) => edit((x) => void (on ? (x.receiveOn = on) : delete x.receiveOn))} />
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
          <Field label="Default voice" hint="Unknown talkgroups, until granted">
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
        <ShareSystem c={c} i={i} />
      </Group>
      <Group
        title="Sites"
        hint={<>Its control channels <Info>Where the recorder finds it: each site's control channel. A call heard on several sites is saved once.</Info></>}
        actions={
          <button className="btn small" onClick={addSite}>
            Add site
          </button>
        }
        cols={1}
      >
        {sys.sites.length > 1 && <PreferredSite sys={sys} edit={edit} />}
        <div className="sites-list">
          {sys.sites.map((x, k) => (
            <SiteCard key={siteKey(sys, x)} c={c} i={i} k={k} onRemoveSystem={() => remove(true)} />
          ))}
          {!sys.sites.length && <p className="muted small">No sites yet</p>}
        </div>
      </Group>
      <FccLicensesGroup
        target={{ kind: "system", index: i }}
        fcc={sys.fcc}
        foldKey={`fcc:${sysKey(sys, i, c.systems)}`}
        hint={<>Who holds each frequency <Info>Who is licensed on each frequency. Motorola P25 sites say their call signs on the air; Identify looks licensees up. Plugins are told them.</Info></>}
      />
      <Group title="Talkgroups and units">
        <TalkgroupsField sys={sys} needs={need(`tg-${sys.shortName}`)} anchor={`tg-${sys.shortName}`} />
        <UnitNamesField
          system={sys.shortName}
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
      <Group title="Plugins" hint="Default: the Plugins tab's" cols={1}>
        <SystemPluginSettings system={i} />
        <SystemDelays system={i} />
        <SystemSignals system={i} />
      </Group>
      <Group title="Recording" hint="Default: the Recording tab's" cols={1}>
        <RecordingOverrides
          c={c}
          sites={sys.sites.length}
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
/** The log (desktop app): Trunk Recorder's options, applied at once. */
function LogPanel(props: { c: Config }) {
  const l: LogSettings = props.c.log ?? defaultLog();
  const set = (patch: Partial<LogSettings>) => updateConfig((x) => void (x.log = { ...l, ...patch }));
  return (
    <section className="set-card">
      <header className="set-card-head">
        <div className="set-card-title">
          <h3>Log</h3>
          <span className="set-card-sub">Live</span>
        </div>
      </header>
      <Group title="Where it goes">
        <Toggle label="To the console" hint="stderr" checked={l.console} onChange={(v) => set({ console: v })} />
        <Toggle label="To the system log" checked={l.syslog} onChange={(v) => set({ syslog: v })} />
        <Toggle label="To files" hint="New daily, or at 100 MB" checked={l.file} onChange={(v) => set({ file: v })} />
        {l.file && (
          <>
            <Field label="Folder" hint="Relative to the config file">
              <TextInput className="mono" value={l.dir} placeholder="logs" onChange={(v) => set({ dir: v })} />
            </Field>
            <Toggle
              label="One file, for logrotate"
              hint="trunk-pro.log"
              info="Never rotated here; SIGHUP reopens it."
              checked={l.syslogFriendly}
              onChange={(v) => set({ syslogFriendly: v })}
            />
          </>
        )}
      </Group>
      <Group title="What it says">
        <Field label="Level" hint="debug: + unrecorded calls · trace: + all control" info="--log-level on the command line overrides it.">
          <select value={l.level} onChange={(e) => set({ level: e.target.value as LogSettings["level"] })}>
            {["trace", "debug", "info", "warning", "error", "fatal"].map((v) => (
              <option key={v} value={v}>
                {v}
              </option>
            ))}
          </select>
        </Field>
        <Field label="Decode rate warning, msg/s" hint="Below it: an error · −1: always">
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
      /** Trunked systems' only (not the conventional channels'). */
      trunked?: boolean;
      /** A change suggested for a system of `sites` sites with these settings, if any. */
      suggest?: (sites: number, r: RecordingRules) => string | undefined;
    }
  | { key: NumRule; kind: "num"; label: string; hint?: string; min: number; max: number; zero?: string }
  | { key: "filenameFormat"; kind: "text"; label: string };

const RULE_GROUPS: { title: string; hint?: string; rules: Rule[] }[] = [
  {
    title: "What is recorded",
    rules: [
      { key: "recordUnknown", kind: "bool", label: "Unlisted talkgroups", hint: "Not in the talkgroup file" },
      { key: "recordUnitToUnit", kind: "bool", label: "Unit-to-unit calls", hint: "Private, radio to radio" },
      { key: "callTimeoutS", kind: "num", label: "Call timeout, s", hint: "Silence that ends a call", min: 1, max: 30 },
      {
        key: "conversationMode",
        kind: "bool",
        label: "Conversations",
        hint: "One call until quiet; off: one per transmission",
        trunked: true,
        suggest: (sites, r) =>
          sites > 1 && r.conversationMode ? "Suggested: Off, for a system with several sites (keeps each transmission's best copy)" : undefined,
      },
    ],
  },
  {
    title: "Which calls are kept",
    rules: [
      { key: "minCallS", kind: "num", label: "Shortest call, s", hint: "Shorter: deleted, not uploaded", min: 0, max: 60, zero: "keep all" },
      {
        key: "minTransmissionS",
        kind: "num",
        label: "Shortest transmission, s",
        hint: "Shorter key-ups dropped",
        min: 0,
        max: 10,
        zero: "keep all",
      },
      { key: "maxCallS", kind: "num", label: "Longest call, s", hint: "Longer: split, nothing lost", min: 0, max: 3600, zero: "no limit" },
      { key: "keepSilentCalls", kind: "bool", label: "Calls with no audio", hint: "Encrypted or undecoded" },
    ],
  },
  {
    title: "Audio",
    rules: [
      { key: "normalizeAudio", kind: "bool", label: "Even out loudness" },
      { key: "digitalLevelDb", kind: "num", label: "Digital level, dB", min: -20, max: 20 },
      { key: "analogLevelDb", kind: "num", label: "Analog level, dB", min: -20, max: 20 },
    ],
  },
  {
    title: "Files",
    rules: [{ key: "filenameFormat", kind: "text", label: "Folders and file names" }],
  },
  {
    // What stays in the recordings folder once the plugins are done with a call (docs/guides/recordings.md).
    title: "Archive",
    hint: "Kept once the plugins are done",
    rules: [
      { key: "archiveWav", kind: "bool", label: "Keep .wav", service: true },
      {
        key: "archiveM4a",
        kind: "bool",
        label: "Keep .m4a",
        hint: "⅒ the WAV's size; needs ffmpeg or afconvert",
        service: true,
        // (An .m4a is made for a plugin that takes one either way: this is only whether it stays.)
        note: (m) => (m.length ? `${m.join(", ")} ${m.length > 1 ? "get" : "gets"} one either way` : undefined),
      },
      { key: "callLog", kind: "bool", label: "Keep call JSON", service: true },
      {
        key: "archiveFilesOnFailure",
        kind: "bool",
        label: "Keep when a plugin fails",
        hint: "Every file, if any plugin fails",
        service: true,
        inactive: (r) => (r.archiveWav && r.archiveM4a && r.callLog ? "Everything is kept anyway" : undefined),
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
            <p className="warn small wide">
              No plugin on: {!(r.archiveWav || r.archiveM4a) && !r.callLog ? "calls" : !r.callLog ? "call JSON" : "call audio"} not saved at all
            </p>
          )}
        </Group>
      ))}
    </section>
  );
}

/** The Recording tab's groups a system's own rules are in, by its card's two overrides. */
const ARCHIVE_GROUPS = ["Files", "Archive"];

/** `sites`: a trunked system's (its rules' suggestions); none for the conventional channels. */
type OverrideProps = { c: Config; value: RecordingOverride | undefined; onChange: (fn: (o: RecordingOverride) => void) => void; sites?: number };

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
  const rules = props.groups.flatMap((g) => g.rules).filter((r) => ruleShown(r) && !(r.kind === "bool" && r.trunked && props.sites === undefined));
  const m4a = useM4aPlugins().map((p) => p.manifest!.name);
  const mine = rules.filter((r) => own[r.key] !== undefined);
  // The settings this system ends up with (its own, else the Recording tab's).
  const effective = { ...base, ...own } as RecordingRules;
  const suggested = (r: Rule) => (r.kind === "bool" && props.sites !== undefined ? r.suggest?.(props.sites, effective) : undefined);
  const suggestions = rules.filter(suggested).length;
  return (
    <details className="help override">
      <summary>
        {props.title}
        <span className="muted">{mine.length ? ` — ${mine.length} changed` : " — defaults"}</span>
        {suggestions > 0 && <span className="warn-text">{` — ${suggestions} suggested`}</span>}
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
                suggested(rule) ||
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

/**
 * Setup → Data (desktop app): where the recorder keeps what it keeps (the
 * call index, talkgroups, aliases, the radio registry, the dashboard's
 * history) and how much it writes. Each takes effect from the next start.
 * What's in it and how big: the dashboard's Platform → Storage.
 */
function DataTab(props: { c: Config }) {
  const st = props.c.storage;
  const set = (patch: Partial<StorageSettings>) => updateConfig((x) => void (x.storage = { backend: "sqlite", url: "", retentionDays: 120, commitSeconds: 300, ...x.storage, ...patch }));
  const low = !!st?.lowWrite;
  const registryRadios = st?.registryRadios ?? 20000;
  // The two optional call indexes: what each finds, what it costs in writes, what's shown without it.
  const indexes = [
    { key: "talkgroupIndex", label: "Talkgroup call history", use: INDEX_USES.calls_system_tg_start, on: st?.talkgroupIndex !== false, cost: "Medium", level: undefined },
    { key: "radioIndex", label: "Radio call history", use: INDEX_USES.transmissions_radio, on: st?.radioIndex !== false, cost: "High", level: "warn" },
  ] as const;
  return (
    <Section title="Data" hint="Changes apply from the next start">
      <section className="set-card">
        <Group
          title="Store"
          hint={
            <>
              Everything but recordings
              <Info>Talkgroups, talker aliases, band plans, the radio registry, the call index and the dashboard&apos;s history. Recordings stay in the recordings folder.</Info>
            </>
          }
        >
          <Field label="Kept in">
            <select value={st?.backend ?? "sqlite"} onChange={(e) => set({ backend: e.target.value as "sqlite" | "postgres" })}>
              <option value="sqlite">This computer (SQLite)</option>
              <option value="postgres">A PostgreSQL server</option>
            </select>
          </Field>
          {st?.backend === "postgres" && (
            <Field label="PostgreSQL connection" hint="One recorder per database or schema" wide>
              <TextInput className="mono" value={st.url ?? ""} placeholder="postgres://user:password@host/database" onChange={(v) => set({ url: v })} />
            </Field>
          )}
        </Group>
        <Group
          title="Writing less"
          hint={
            <>
              For SD cards and eMMC
              <Info>Each index and commit is writes a card wears out by.</Info>
            </>
          }
        >
          <div className="field wide">
            <span className="field-label">Writes</span>
            <div className="seg small" role="radiogroup" aria-label="Writes">
              <button role="radio" aria-checked={!low} className={low ? "" : "on"} onClick={() => set({ lowWrite: undefined })}>
                Normal
              </button>
              <button role="radio" aria-checked={low} className={low ? "on" : ""} onClick={() => set({ lowWrite: true })}>
                Low-write
              </button>
            </div>
            {low && (
              <Figs
                inline
                items={[
                  { k: "Call lists", v: "Recent calls only", title: "No call index" },
                  { k: "Dashboard history", v: "Hourly" },
                  { k: "Commit", v: "Hourly" },
                ]}
              />
            )}
          </div>
          {!low &&
            indexes.map((x) => (
              <div className="field wide index-switch" key={x.key}>
                <div className="row">
                  <Switch label={x.label} checked={x.on} onChange={(v) => set({ [x.key]: v ? undefined : false })} />
                  <Info label={x.label}>
                    Finds {x.use.finds}. Used by {x.use.used}. On a busy system the radio index is most of what the store writes, the talkgroup index about a third as much.
                  </Info>
                </div>
                <Figs
                  inline
                  items={[
                    { k: "Writes", v: x.cost, level: x.level },
                    { k: "Off", v: "Recent calls only", title: `Off: ${x.use.without}` },
                  ]}
                />
              </div>
            ))}
          <Field label="Write every, s" hint="Longer: fewer writes" info="Calls show at once either way.">
            <NumberInput label="Write every, s" value={st?.commitSeconds ?? 300} onChange={(v) => set({ commitSeconds: Math.max(10, Math.min(3600, Math.round(v ?? 300))) })} />
          </Field>
        </Group>
        <Group
          title="Radio registry"
          hint={
            <>
              Talkgroups and radios heard
              <Info>
                Every talkgroup, radio and pair of radios each system has heard (the Radio page, first-heard alerts). Held in memory, about 2 kB a radio, and saved as it changes. Past either
                limit, what was heard least recently is forgotten, and comes back as new if heard again.
              </Info>
            </>
          }
        >
          <Field
            label="Radios kept, per system"
            hint={registryRadios > 0 ? <>≈ <Fig v={Math.max(1, Math.round((registryRadios * 2) / 1000))} u="MB memory" /> · 0: no limit</> : "No limit but the days"}
            info="Talkgroups: a quarter as many, 5,000 at least."
          >
            <NumberInput label="Radios kept, per system" value={registryRadios} onChange={(v) => set({ registryRadios: Math.max(0, Math.round(v ?? 20000)) })} />
          </Field>
          <Field label="Forget after, days unheard" hint="0: never" info="A talkgroup, radio or pair not heard for this long.">
            <NumberInput label="Forget after, days unheard" value={st?.registryDays ?? 365} onChange={(v) => set({ registryDays: Math.max(0, Math.round(v ?? 365)) })} />
          </Field>
        </Group>
        <Group title="How long">
          <Field label="Call history, days" hint="0: forever · files untouched" info="How long calls stay findable by talkgroup and radio. Their files aren't touched.">
            <NumberInput label="Call history, days" value={st?.retentionDays ?? 120} onChange={(v) => set({ retentionDays: Math.max(0, Math.round(v ?? 120)) })} />
          </Field>
          <Field label="Dashboard history, days" hint="Hourly · 0: forever" info="How far back the dashboard's charts go, kept by the hour.">
            <NumberInput label="Dashboard history, days" value={st?.statsDays ?? 365} onChange={(v) => set({ statsDays: Math.max(0, Math.round(v ?? 365)) })} />
          </Field>
        </Group>
      </section>
      <ManageData />
    </Section>
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
  { id: "data", label: "Data", icon: IconDatabase },
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
    data: null,
    plugins: s.plugins ? s.plugins.plugins.length : null,
  };
  return (
    <nav className="setup-tabs" role="tablist" aria-label="Setup">
      {TABS.filter((t) => (t.id !== "plugins" || can("plugins")) && (t.id !== "data" || !web)).map((t) => {
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
      <SaveStatus />
    </nav>
  );
}

/**
 * Whether this browser's changes are saved: each setting typed is saved once
 * it's done (left, or Enter), each switch at once, so this says it took.
 */
function SaveStatus() {
  const s = useSelect((x) => ({ status: x.saveStatus, error: x.saveError }));
  if (s.status === "idle") return null;
  const text = { saving: "Saving…", saved: "All changes saved", failed: "Couldn't save" }[s.status];
  return (
    <span className={`save-status ${s.status}`} role="status" title={s.error ?? undefined}>
      {text}
    </span>
  );
}

export function Setup() {
  const s = useApp();
  const c = s.config;
  const tab = s.setupTab;
  shownTab = tab;
  // A source just added on the Radios tab (measuring its roll-off is offered).
  const [freshSource, setFreshSource] = useState<number | null>(null);
  // Better centres and rates for the sources, while they're shown.
  const advice = useSourceAdvice(tab === "radios" ? c : null);
  if (!c) return <p className="muted">Connecting to the recorder…</p>;

  return (
    <div className="setup">
      <TodoPanel />
      <SetupTabs c={c} tab={tab} />
      {tab === "systems" && (
        <>
          <Section
            title="Trunked systems"
            hint="P25, SmartNet, DMR, NXDN and EDACS"
            actions={
              <>
                <ExpandAll keys={c.systems.map(sysKey)} />
                <button className="btn ghost" onClick={() => openGuide("import")} title="A Trunk Recorder config, or a system downloaded from the directory">
                  Import…
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
          <SourcesAdvice advice={advice} />
          {c.sources.map((x, i) => (
            <SourceCard key={srcKey(x, i, c.sources)} c={c} i={i} advice={advice} fresh={freshSource === i} onFreshDone={() => setFreshSource(null)} />
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
        <Section title="Recorder">
          <section className="set-card">
            <Group title="Storage">
              {web ? (
                <p className="muted small wide">
                  In this browser; export from <i>Recorded calls</i>
                </p>
              ) : (
                <>
                  <Field label="Recordings folder" wide>
                    <TextInput className="mono" value={c.recording.captureDir} onChange={(v) => updateConfig((x) => void (x.recording.captureDir = v))} />
                  </Field>
                  <Toggle
                    label="RAM spool"
                    hint="Upload queue in memory; from next start"
                    info="Calls waiting to upload go to a RAM disk, not the recordings folder: an SSD is spared the writes of calls uploaded, then deleted."
                    checked={c.recording.ramSpool?.enabled ?? false}
                    onChange={(v) => updateConfig((x) => void (x.recording.ramSpool = { sizeMb: 256, ...x.recording.ramSpool, enabled: v }))}
                  />
                  {c.recording.ramSpool?.enabled && (
                    <Field label="RAM spool size, MB" hint="Overflow: the recordings folder">
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
            <Group title="Recording" cols={3}>
              <Field label="Recorders" hint="Calls recorded at once">
                <NumberInput label="Recorders" value={c.recording.maxRecorders} onChange={(v) => updateConfig((x) => void (x.recording.maxRecorders = Math.max(1, Math.min(64, Math.round(v || 32)))))} />
              </Field>
              <Field label="Pre-roll, s" hint="Audio before each grant">
                <NumberInput label="Pre-roll, s" value={c.recording.prerollS} onChange={(v) => updateConfig((x) => void (x.recording.prerollS = Math.max(0, Math.min(3, v || 0))))} />
              </Field>
              <Field label="P25 voice decoder">
                <select value={c.recording.vocoder ?? "fixed"} onChange={(e) => updateConfig((x) => void (x.recording.vocoder = e.target.value as "fixed" | "enhanced" | "mbelib"))}>
                  <option value="fixed">Fixed-point (most natural)</option>
                  <option value="enhanced">Enhanced</option>
                  <option value="mbelib">mbelib</option>
                </select>
              </Field>
              <Toggle
                label="One copy per multi-site call"
                hint="The cleanest, or the Preferred Site's"
                checked={c.recording.dropDuplicateCalls ?? true}
                onChange={(v) => updateConfig((x) => void (x.recording.dropDuplicateCalls = v))}
              />
            </Group>
            {can("service") && (
              <Group title="Startup and diagnostics">
                <Toggle label="Record when the app starts" checked={c.server.autoStart} onChange={(v) => updateConfig((x) => void (x.server.autoStart = v))} />
                <Toggle
                  label="Save vocoder frames"
                  hint="<call>.sdr, for diagnostics and MimoSDR"
                  checked={c.recording.captureFrames}
                  onChange={(v) => updateConfig((x) => void (x.recording.captureFrames = v))}
                />
              </Group>
            )}
          </section>
        </Section>
      )}
      {tab === "recording" && can("keys") && s.lockable.length > 0 && (
        <Section
          title="Key containers"
          hint={
            <>
              KFDtool containers without a password file
              <Info>Unlocked keys stay in memory until you lock them or the app quits; the password is never saved.</Info>
            </>
          }
        >
          {s.lockable.map((k) => (
            <KeyContainer key={k.name} k={k} />
          ))}
        </Section>
      )}
      {tab === "recording" && (
        <Section title="Call rules" hint="Defaults; each system can override">
          <CallRules c={c} />
        </Section>
      )}
      {tab === "recording" && can("service") && (
        <Section title="Advanced">
          <M4aSettings />
          <LogPanel c={c} />
        </Section>
      )}

      {tab === "data" && !web && <DataTab c={c} />}
      {tab === "plugins" && can("plugins") && <PluginSetupPanel />}
    </div>
  );
}
