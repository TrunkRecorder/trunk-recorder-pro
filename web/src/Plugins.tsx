// Plugins (desktop app). The Plugins page manages them: the installed ones,
// how each is doing while recording, its log, updates and uninstalling, and
// the plugin store (the registry's plugins, to install). They're set up in
// Setup, like the rest of the recorder: on or off and their settings for the
// whole recorder on its Plugins tab, their settings for each system on that
// system's card. All of it is in the config, saved as it's typed; while
// recording, plugins restart when their settings change.
//
// The settings forms are drawn from the schemas plugins describe themselves
// with (trunk-recorder-plugin's schema.rs lists the subset).

import { useEffect, useRef, useState } from "react";
import {
  addPlugin,
  fetchPluginStore,
  installPlugin,
  installPluginFrom,
  pluginOn,
  removePlugin,
  setM4a,
  setPluginEnabled,
  setPluginSettings,
  setSystemPluginSettings,
  settingsOpened,
  setView,
  systemAt,
  useApp,
  type AppState,
  type SystemRef,
} from "./controller.ts";
import { IconPuzzle, IconUpload, IconWave } from "./Onboarding.tsx";
import { showTodo } from "./Setup.tsx";
import { openTodos } from "./todo.ts";
import type { Config, PluginInfo, PluginInstall, PluginManifest, PluginSchema, PluginStore, PluginValues, PluginsList, StoreListing } from "./protocol.ts";

/** Is version `a` newer than `b`? Semver: a prerelease is older than its release. */
function newer(a: string, b: string): boolean {
  const parse = (v: string) => {
    const [core, pre] = v.replace(/^v/, "").split("+")[0].split(/-(.*)/s);
    const n = core.split(".").map(Number);
    return n.length === 3 && n.every(Number.isInteger) ? { n, pre: pre ?? null } : null;
  };
  const x = parse(a);
  const y = parse(b);
  if (!x || !y) return false;
  for (let i = 0; i < 3; i++) if (x.n[i] !== y.n[i]) return x.n[i] > y.n[i];
  if (x.pre === null || y.pre === null) return x.pre === null && y.pre !== null;
  return x.pre > y.pre;
}

const STAGES: Record<PluginInstall["stage"], string> = {
  finding: "Finding it…",
  downloading: "Downloading…",
  checking: "Checking it…",
  installing: "Installing…",
  done: "Installed",
  failed: "Didn't install",
};

/** The tier, as a chip. */
function TierChip(props: { tier: StoreListing["tier"] | "unreviewed"; title?: string }) {
  switch (props.tier) {
    case "official":
      return (
        <span className="chip ok" title={props.title ?? "Made with the recorder, and reviewed"}>
          Official
        </span>
      );
    case "community":
      return (
        <span className="chip" title={props.title ?? "Made by someone else, and reviewed for the registry"}>
          Community
        </span>
      );
    default:
      return (
        <span className="chip warn" title={props.title ?? "Not from the plugin registry: nobody has reviewed it"}>
          Not reviewed
        </span>
      );
  }
}

/** "apiKey" → "Api key": a label for a field the plugin didn't title. */
function spell(key: string): string {
  const words = key
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/[_-]+/g, " ")
    .trim()
    .toLowerCase();
  return words.charAt(0).toUpperCase() + words.slice(1);
}

function fieldsOf(schema: PluginSchema | undefined): [string, PluginSchema][] {
  const props = schema?.properties ?? {};
  const order = schema?.["x-order"] ?? Object.keys(props);
  return order.filter((k) => props[k]).map((k) => [k, props[k]]);
}

const hasFields = (schema: PluginSchema | undefined) => fieldsOf(schema).length > 0;

const emptyValue = (v: unknown) => v === undefined || v === null || v === "" || (Array.isArray(v) && v.length === 0);

/** The fields an object's schema says have to be filled in. */
function requiredOf(schema: PluginSchema | undefined): string[] {
  const marked = fieldsOf(schema)
    .filter(([, f]) => f["x-required"])
    .map(([k]) => k);
  return [...new Set([...(schema?.required ?? []), ...marked])];
}

/** The labels of required fields left empty (a value for every system counts). */
function missing(schema: PluginSchema | undefined, values: PluginValues | undefined, inherited?: PluginValues): string[] {
  return requiredOf(schema)
    .filter((k) => emptyValue(values?.[k]) && emptyValue(inherited?.[k]))
    .map((k) => schema?.properties?.[k]?.title ?? spell(k));
}

/** Is a system set up for a plugin? Its required fields are filled in; when
 * the plugin marks none, it has a setting of its own (or one for every system). */
function systemSetUp(m: PluginManifest, values: PluginValues | undefined, inherited: PluginValues | undefined): boolean {
  if (requiredOf(m.system_config).length) return missing(m.system_config, values, inherited).length === 0;
  return fieldsOf(m.system_config).some(([k]) => !emptyValue(values?.[k]) || !emptyValue(inherited?.[k]));
}

/** The systems plugins can be set up for: where each is, and its short name (conventional ones with channels). */
function systemsOf(c: Config): { at: SystemRef; name: string; values: (id: string) => PluginValues | undefined }[] {
  const out: { at: SystemRef; name: string; values: (id: string) => PluginValues | undefined }[] = c.systems.map((x, i) => ({
    at: i,
    name: x.shortName,
    values: (id: string) => x.plugins?.[id],
  }));
  c.conventional.forEach((v, k) => {
    if (v.channels.length) out.push({ at: { conv: k }, name: v.shortName, values: (id) => v.plugins?.[id] });
  });
  return out;
}

/** A system was renamed: plugin settings that name it (`x-system` fields) follow. Changes `x`. */
export function renameSystemRefs(x: Config, from: string, to: string, plugins: PluginInfo[]): void {
  if (!from || from === to) return;
  const walk = (schema: PluginSchema | undefined, v: unknown) => {
    if (!v || typeof v !== "object") return;
    const obj = v as PluginValues;
    for (const [k, f] of fieldsOf(schema)) {
      if (f["x-system"] && obj[k] === from) obj[k] = to;
      else if (f.type === "object") walk(f, obj[k]);
      else if (f.type === "array" && f.items?.type === "object" && Array.isArray(obj[k])) for (const item of obj[k] as unknown[]) walk(f.items, item);
    }
  };
  for (const p of plugins) {
    if (!p.manifest) continue;
    walk(p.manifest.config, x.plugins?.[p.id]?.settings);
    for (const sys of [...x.systems, ...x.conventional]) walk(p.manifest.system_config, sys.plugins?.[p.id]);
  }
}

function isUrl(s: string): boolean {
  try {
    const u = new URL(s);
    return u.protocol === "http:" || u.protocol === "https:";
  } catch {
    return false;
  }
}

/** One setting. Empty text means "not set": the value for every system
 * (`inherited`), else the plugin's default, applies — shown as the placeholder. */
function SchemaField(props: { name: string; schema: PluginSchema; value: unknown; onChange: (v: unknown) => void; id: string; inherited?: unknown; required?: boolean }) {
  const { schema: f, value, onChange, id, inherited } = props;
  const label = f.title ?? spell(props.name);
  const hint = f.description;
  const [shown, setShown] = useState(false);
  const c = useApp().config;
  const fallback = emptyValue(inherited) ? f.default : inherited;
  const needed = !!props.required && emptyValue(value) && emptyValue(inherited);

  if (f.type === "object") {
    const obj = (value && typeof value === "object" ? value : {}) as PluginValues;
    return (
      <fieldset className="plugin-group">
        <legend>{label}</legend>
        {hint && <p className="field-hint">{hint}</p>}
        <SchemaFields schema={f} values={obj} onChange={(v) => onChange(v)} id={id} />
      </fieldset>
    );
  }
  if (f.type === "array" && f.items?.type === "object") {
    const items = (Array.isArray(value) ? value : []) as PluginValues[];
    const itemLabel = f.items.title ?? "Item";
    const set = (next: PluginValues[]) => onChange(next.length ? next : undefined);
    return (
      <fieldset className="plugin-group">
        <legend>{label}</legend>
        {hint && <p className="field-hint">{hint}</p>}
        <div className="stack">
          {items.map((item, i) => (
            <fieldset className="plugin-group" key={i}>
              <legend>
                {itemLabel} {i + 1}
              </legend>
              <SchemaFields schema={f.items} values={item ?? {}} onChange={(v) => set(items.map((x, j) => (j === i ? v : x)))} id={`${id}-${i}`} />
              <button type="button" className="btn ghost small" onClick={() => set(items.filter((_, j) => j !== i))}>
                Remove {itemLabel.toLowerCase()} {i + 1}
              </button>
            </fieldset>
          ))}
          <div>
            <button type="button" className="btn small" onClick={() => set([...items, {}])}>
              Add {itemLabel.toLowerCase()}
            </button>
          </div>
        </div>
      </fieldset>
    );
  }
  if (f.type === "boolean") {
    const on = typeof value === "boolean" ? value : fallback === true;
    return (
      <label className="toggle" htmlFor={id}>
        <input id={id} type="checkbox" checked={on} onChange={(e) => onChange(e.target.checked)} />
        <span>
          {label}
          {hint && <span className="field-hint"> — {hint}</span>}
        </span>
      </label>
    );
  }

  let control: React.ReactNode;
  let problem: string | null = null;
  if (f["x-system"]) {
    // A menu of the systems (and one no longer there, if it names one).
    const names = c ? systemsOf(c).map((x) => x.name) : [];
    const cur = typeof value === "string" ? value : "";
    control = (
      <select id={id} value={cur} onChange={(e) => onChange(e.target.value === "" ? undefined : e.target.value)}>
        <option value="">{typeof fallback === "string" && fallback ? `As for every system (${fallback})` : props.required ? "Choose a system" : "Every system"}</option>
        {names.map((n) => (
          <option key={n} value={n}>
            {n}
          </option>
        ))}
        {cur && !names.includes(cur) && <option value={cur}>{cur} (no such system)</option>}
      </select>
    );
    if (cur && !names.includes(cur)) problem = "No system has this name any more";
  } else if (f.enum) {
    const labels = f["x-enum-labels"];
    const cur = value ?? fallback ?? f.enum[0];
    control = (
      <select id={id} value={String(cur)} onChange={(e) => onChange(f.enum!.find((x) => String(x) === e.target.value))}>
        {f.enum.map((x, i) => (
          <option key={String(x)} value={String(x)}>
            {labels?.[i] ?? String(x)}
          </option>
        ))}
      </select>
    );
  } else if (f.type === "integer" || f.type === "number") {
    const text = typeof value === "number" ? String(value) : "";
    control = (
      <input
        id={id}
        className="mono"
        type="number"
        min={f.minimum}
        max={f.maximum}
        step={f.type === "integer" ? 1 : "any"}
        value={text}
        placeholder={fallback !== undefined && fallback !== null ? String(fallback) : ""}
        onChange={(e) => onChange(e.target.value === "" ? undefined : Number(e.target.value))}
      />
    );
    if (typeof value === "number") {
      if (f.type === "integer" && !Number.isInteger(value)) problem = "A whole number";
      else if (f.minimum !== undefined && value < f.minimum) problem = `At least ${f.minimum}`;
      else if (f.maximum !== undefined && value > f.maximum) problem = `At most ${f.maximum}`;
    }
  } else if (f.type === "array") {
    const items = Array.isArray(value) ? value : [];
    const numeric = f.items?.type === "integer" || f.items?.type === "number";
    control = (
      <input
        id={id}
        className={numeric ? "mono" : ""}
        value={items.join(", ")}
        placeholder={Array.isArray(fallback) && fallback.length ? fallback.join(", ") : "Separate with commas"}
        onChange={(e) => {
          const parts = e.target.value.split(",").map((x) => x.trim());
          const vals = numeric ? parts.filter((x) => x !== "").map(Number) : parts.filter((x, i) => x !== "" || i === parts.length - 1);
          onChange(vals.length ? vals : undefined);
        }}
      />
    );
    if (numeric && items.some((x) => typeof x === "number" && Number.isNaN(x))) problem = "Numbers, separated with commas";
  } else {
    const text = typeof value === "string" ? value : "";
    const placeholder = typeof fallback === "string" ? fallback : "";
    const set = (v: string) => onChange(v === "" ? undefined : v);
    if (f["x-multiline"]) {
      control = <textarea id={id} rows={4} value={text} placeholder={placeholder} onChange={(e) => set(e.target.value)} />;
    } else if (f["x-secret"]) {
      control = (
        <span className="secret">
          <input id={id} className="mono" type={shown ? "text" : "password"} autoComplete="off" value={text} placeholder={placeholder} onChange={(e) => set(e.target.value)} />
          <button type="button" className="btn ghost small" onClick={() => setShown(!shown)} aria-label={shown ? `Hide ${label}` : `Show ${label}`}>
            {shown ? "Hide" : "Show"}
          </button>
        </span>
      );
    } else {
      control = <input id={id} type={f.format === "uri" ? "url" : "text"} value={text} placeholder={placeholder} onChange={(e) => set(e.target.value)} />;
    }
    if (f.format === "uri" && text && !isUrl(text)) problem = "A web address, like https://example.com";
  }

  return (
    <div className="field">
      <label className="field-label" htmlFor={id}>
        {label}
      </label>
      {control}
      {problem ? (
        <span className="field-hint bad-text">{problem}</span>
      ) : (
        <>
          {hint && <span className="field-hint">{hint}</span>}
          {needed && <span className="field-hint warn-text">Needed</span>}
        </>
      )}
    </div>
  );
}

function SchemaFields(props: { schema: PluginSchema | undefined; values: PluginValues; onChange: (v: PluginValues) => void; id: string; inherited?: PluginValues }) {
  const required = requiredOf(props.schema);
  return (
    <div className="stack plugin-fields">
      {fieldsOf(props.schema).map(([k, f]) => (
        <SchemaField
          key={k}
          name={k}
          schema={f}
          id={`${props.id}-${k}`}
          value={props.values[k]}
          inherited={props.inherited?.[k]}
          required={required.includes(k)}
          onChange={(v) => {
            const next = { ...props.values };
            if (v === undefined) delete next[k];
            else next[k] = v;
            props.onChange(next);
          }}
        />
      ))}
    </div>
  );
}

/** On a system's card in Setup: its settings for each plugin that's on and
 * takes some (an upload service's key for this system, say). */
export function SystemPluginSettings(props: { system: SystemRef }) {
  const s = useApp();
  const c = s.config;
  if (!c || !s.plugins) return null;
  const sys = systemAt(c, props.system);
  const plugins = s.plugins.plugins.filter((p) => pluginOn(c, p.id) && p.manifest && hasFields(p.manifest.system_config));
  if (!sys || !sys.shortName || plugins.length === 0) return null;
  return (
    <div className="system-plugins stack" id={typeof props.system === "number" ? `need-sysplug-${sys.shortName}` : `need-convplug-${sys.shortName}`}>
      <h4>Plugins</h4>
      {plugins.map((p) => {
        const m = p.manifest!;
        const values = sys.plugins?.[p.id];
        const every = c.plugins?.[p.id]?.settings;
        return (
          <fieldset className="plugin-group" key={p.id}>
            <legend>{m.name}</legend>
            {!systemSetUp(m, values, every) && <p className="field-hint warn-text">Not set up for {sys.shortName} yet.</p>}
            <SchemaFields schema={m.system_config} values={values ?? {}} inherited={every} onChange={(v) => setSystemPluginSettings(props.system, p.id, v)} id={`sp-${p.id}-${sys.shortName}`} />
          </fieldset>
        );
      })}
    </div>
  );
}

/** Setup's Plugins tab: each installed plugin, to turn on and set up. */
export function PluginSetupPanel() {
  const s = useApp();
  const list = s.plugins;
  const c = s.config;
  if (!c) return null;
  const plugins = [...(list?.plugins ?? [])].sort((a, b) => (a.manifest?.name ?? a.id).localeCompare(b.manifest?.name ?? b.id));
  return (
    <section className="panel" aria-labelledby="setup-plugins" id="need-plugins">
      <header className="panel-head">
        <h2 id="setup-plugins">Plugins</h2>
        <button className="btn ghost" onClick={() => setView("plugins")}>
          Find and install plugins…
        </button>
      </header>
      <p className="muted small plugins-intro">
        Plugins upload calls to services like OpenMHz, stream audio, or send alerts, while the recorder records. Turn them on and set them up here; what each needs for
        a system is on that system's card. Install, update and remove them on the Plugins page.
      </p>
      {!list ? (
        <p className="muted">Asking the recorder about plugins…</p>
      ) : (
        <div className="stack">
          {plugins.map((p) => (
            <PluginSetupCard key={p.id} p={p} c={c} />
          ))}
          {plugins.length === 0 && (
            <p className="empty">
              No plugins installed yet.{" "}
              <button className="btn ghost small" onClick={() => setView("plugins")}>
                Find plugins
              </button>
            </p>
          )}
        </div>
      )}
    </section>
  );
}

function PluginSetupCard(props: { p: PluginInfo; c: Config }) {
  const { p, c } = props;
  const s = useApp();
  const m = p.manifest;
  const on = pluginOn(c, p.id);
  const settings = c.plugins?.[p.id]?.settings;
  const recording = s.phase === "running" || s.phase === "starting";
  const st = stateOf(p, on, recording);
  const imported = openTodos(s).find((t) => t.target === `plugin-${p.id}`);
  const needs = m ? missing(m.config, settings) : [];
  const systems = systemsOf(c);
  const perSystem = !!m && hasFields(m.system_config);
  return (
    <article className={`plugin-card${on ? "" : " off"}${imported ? " needs" : ""}`} aria-labelledby={`ps-${p.id}`} id={`need-plugin-${p.id}`}>
      {imported && <div className="field-needs">{imported.text}</div>}
      <PluginHead p={p} on={on} tone={st.tone} state={needs.length && on ? "Needs setting up" : st.text} titleId={`ps-${p.id}`} />
      {m?.description && <p className="plugin-desc">{m.description}</p>}
      {p.problem && <p className="small bad-text">{p.problem}</p>}
      {m && hasFields(m.config) && (
        <div className="plugin-settings stack">
          <SchemaFields schema={m.config} values={settings ?? {}} onChange={(v) => setPluginSettings(p.id, v)} id={`pg-${p.id}`} />
        </div>
      )}
      {m && perSystem && (
        <div className="plugin-systems small">
          <span className="muted">For each system:</span>{" "}
          {systems.length === 0 ? (
            <span className="muted">no systems yet.</span>
          ) : !on ? (
            <span className="muted">turn it on to set it up on each system's card.</span>
          ) : (
            systems.map((x) => {
              const ok = systemSetUp(m, x.values(p.id), settings);
              return (
                <button
                  key={x.name}
                  className={`chip ${ok ? "ok" : "warn"}`}
                  title={ok ? `Set up for ${x.name}` : `Not set up for ${x.name} yet`}
                  onClick={() => showTodo(typeof x.at === "number" ? `sysplug-${x.name}` : `convplug-${x.name}`)}
                >
                  {ok ? "✓" : "!"} {x.name}
                </button>
              );
            })
          )}
        </div>
      )}
      {recording && on && <p className="small muted">Changes restart it.</p>}
    </article>
  );
}

/** A plugin's name, version, where it came from, state and on/off switch. */
function PluginHead(props: { p: PluginInfo; on: boolean; tone: string; state: string; titleId: string; extra?: React.ReactNode }) {
  const { p, on } = props;
  const m = p.manifest;
  return (
    <div className="plugin-head">
      <span className={`plugin-icon tone-${props.tone}`} aria-hidden="true">
        {/* What it does: streams live audio, uploads calls, or something else. */}
        {m?.subscribe.includes("audio") ? <IconWave /> : m?.subscribe.includes("call.concluded") ? <IconUpload /> : <IconPuzzle />}
      </span>
      <div className="plugin-title">
        <div className="row">
          <strong id={props.titleId}>{m?.name ?? p.id}</strong>
          {m && <span className="chip">v{m.version}</span>}
          {p.custom && (
            <span className="chip" title={p.path}>
              your build
            </span>
          )}
          {p.unlistedFrom && <TierChip tier="unreviewed" title={`Installed from ${p.unlistedFrom}, not the plugin registry: nobody has reviewed it`} />}
          {props.extra}
        </div>
        <span className={`plugin-state tone-${props.tone}`}>{props.state}</span>
      </div>
      <label className="switch" title={p.problem ? "It can't run: see below" : ""}>
        <input type="checkbox" role="switch" checked={on} disabled={!!p.problem && !on} onChange={(e) => setPluginEnabled(p.id, e.target.checked)} />
        <span className="switch-track" aria-hidden="true" />
        <span>{on ? "On" : "Off"}</span>
      </label>
    </div>
  );
}

/** M4A for the plugins that upload it (Setup's Recording tab). */
export function M4aSettings() {
  const s = useApp();
  const c = s.config;
  const a = c?.recording.m4a ?? { encoder: "auto", bitrateKbps: 32 };
  const found = s.plugins?.encoderFound ?? null;
  const wanted = (s.plugins?.plugins ?? []).filter((p) => pluginOn(c, p.id) && p.manifest?.audio_formats.includes("m4a"));
  const [kbps, setKbps] = useState(String(a.bitrateKbps));
  useEffect(() => setKbps(String(a.bitrateKbps)), [a.bitrateKbps]);
  return (
    <section className="panel" aria-labelledby="m4a-title">
      <header className="panel-head">
        <h2 id="m4a-title">M4A audio</h2>
        {found ? <span className="chip ok">{found}</span> : <span className={`chip ${wanted.length ? "bad" : ""}`}>{a.encoder === "none" ? "off" : "no encoder found"}</span>}
      </header>
      <div className="stack">
        <p className="muted small">
          M4A is about a tenth the size of WAV. Some plugins upload it, and <b>Also save an M4A</b> (Call rules) keeps one of every call. The recorder encodes each
          call once, with an encoder already on this computer.{" "}
          {wanted.length > 0 && !found && (
            <span className="bad-text">
              {wanted.map((p) => p.manifest!.name).join(", ")} {wanted.length > 1 ? "need" : "needs"} it: install ffmpeg.
            </span>
          )}
        </p>
        <div className="grid2">
          <div className="field">
            <label className="field-label" htmlFor="m4a-encoder">
              Encoder
            </label>
            <select id="m4a-encoder" value={a.encoder} onChange={(e) => setM4a({ encoder: e.target.value })}>
              <option value="auto">Automatic (ffmpeg, then afconvert, then fdkaac)</option>
              <option value="ffmpeg">ffmpeg</option>
              <option value="afconvert">afconvert (macOS)</option>
              <option value="fdkaac">fdkaac</option>
              <option value="none">None: WAV only</option>
            </select>
          </div>
          <div className="field">
            <label className="field-label" htmlFor="m4a-kbps">
              Bitrate, kbps
            </label>
            <input
              id="m4a-kbps"
              className="mono"
              type="number"
              min={8}
              max={320}
              value={kbps}
              onChange={(e) => setKbps(e.target.value)}
              onBlur={() => {
                const k = Math.round(Number(kbps));
                if (k >= 8 && k <= 320 && k !== a.bitrateKbps) setM4a({ bitrateKbps: k });
                else setKbps(String(a.bitrateKbps));
              }}
            />
            <span className="field-hint">32 is what Trunk Recorder uses.</span>
          </div>
        </div>
      </div>
    </section>
  );
}

/** How a plugin is doing, in a word or two, and its tone. */
function stateOf(p: PluginInfo, on: boolean, recording: boolean): { tone: "ok" | "warn" | "bad" | "off"; text: string } {
  if (p.problem) return { tone: "bad", text: "Can't run" };
  if (!on) return { tone: "off", text: "Off" };
  const r = p.runtime;
  if (!recording) return r.state === "error" && r.message ? { tone: "bad", text: "Stopped with a problem" } : { tone: "off", text: "Runs while recording" };
  switch (r.state) {
    case "ok":
      return { tone: "ok", text: "Running" };
    case "warning":
      return { tone: "warn", text: "Needs attention" };
    case "error":
      return { tone: "bad", text: "Not running" };
    case "starting":
      return { tone: "warn", text: "Starting…" };
    default:
      return { tone: "off", text: "Not running" };
  }
}

/** An installed plugin on the Plugins page: how it's doing, its log, updates; "Set up" goes to Setup. */
function PluginCard(props: { p: PluginInfo; recording: boolean; listing?: StoreListing; install?: PluginInstall }) {
  const { p, recording, listing, install } = props;
  const m = p.manifest;
  const s = useApp();
  const on = pluginOn(s.config, p.id);
  const st = stateOf(p, on, recording);
  const r = p.runtime;
  const message = p.problem ?? (on && r.message && r.message !== "running" ? r.message : "");
  const counted = r.ok + r.skipped + r.failed > 0;
  const needsM4a = m?.audio_formats.includes("m4a");
  const needs = on && m ? missing(m.config, s.config?.plugins?.[p.id]?.settings) : [];
  // A newer version in the registry (not for a build of the user's own, or a plugin from elsewhere).
  const update = listing && m && !p.custom && !p.unlistedFrom && !listing.unavailable && newer(listing.version, m.version) ? listing : null;

  // Just installed from this browser: on to setting it up.
  useEffect(() => {
    if (s.pluginJustInstalled !== p.id) return;
    settingsOpened();
    showTodo(`plugin-${p.id}`);
  }, [s.pluginJustInstalled, p.id]);

  return (
    <article className={`plugin-card${on ? "" : " off"}`} aria-labelledby={`pn-${p.id}`}>
      <PluginHead
        p={p}
        on={on}
        tone={st.tone}
        state={needs.length ? "Needs setting up" : st.text}
        titleId={`pn-${p.id}`}
        extra={
          <>
            {listing && !p.custom && !p.unlistedFrom && <TierChip tier={listing.tier} />}
            {update && <span className="chip warn">v{update.version} available</span>}
          </>
        }
      />
      {m?.description && <p className="plugin-desc">{m.description}</p>}
      {message && <p className={`small ${st.tone === "bad" ? "bad-text" : st.tone === "warn" ? "warn-text" : "muted"}`}>{message}</p>}
      {counted && (
        <p className="small muted" title={r.lastFailure || undefined}>
          {recording ? "This recording" : "Last recording"}: {r.ok} done
          {r.skipped > 0 && ` · ${r.skipped} skipped`}
          {r.failed > 0 && <span className="bad-text"> · {r.failed} failed</span>}
          {r.failed > 0 && r.lastFailure && <span className="plugin-last-failure"> — last: {r.lastFailure}</span>}
        </p>
      )}
      {m && (
        <p className="small muted plugin-meta">
          {needsM4a && "Uploads M4A audio · "}
          {m.subscribe.length ? `Hears ${m.subscribe.map((t) => TOPIC_NAMES[t] ?? t).join(", ")}` : "Hears nothing"}
          {(m.homepage || m.repository) && (
            <>
              {" · "}
              <a href={m.homepage || m.repository} target="_blank" rel="noreferrer">
                {m.homepage ? "Website" : "Source"}
              </a>
            </>
          )}
        </p>
      )}
      <div className="row plugin-actions">
        {m && (
          <button className="btn" onClick={() => showTodo(`plugin-${p.id}`)}>
            Set up
          </button>
        )}
        {r.log.length > 0 && <PluginLog p={p} />}
        {install ? (
          <span className="small muted install-stage" role="status">
            {STAGES[install.stage]}
          </span>
        ) : (
          update && (
            <button className="btn primary" onClick={() => installPlugin(p.id)} title={recording && on ? "It restarts with the new version." : undefined}>
              Update to v{update.version}
            </button>
          )
        )}
        <span className="spacer" />
        <button
          className="btn ghost small danger"
          disabled={!!install}
          onClick={() => {
            const what = p.custom
              ? `Remove ${m?.name ?? p.id} from the recorder? Its settings are forgotten; your build at ${p.path} isn't touched.`
              : `Uninstall ${m?.name ?? p.id}? Its settings, for every system too, are forgotten.`;
            if (confirm(what)) removePlugin(p.id);
          }}
        >
          {p.custom ? "Remove" : "Uninstall"}
        </button>
      </div>
    </article>
  );
}

const TOPIC_NAMES: Record<string, string> = {
  "call.concluded": "recorded calls",
  "call.start": "calls starting",
  "call.end": "calls ending",
  unit: "radio activity",
  audio: "live audio",
  status: "system status",
};

function PluginLog(props: { p: PluginInfo }) {
  const [open, setOpen] = useState(false);
  const lines = props.p.runtime.log;
  return (
    <>
      <button className="btn ghost" aria-expanded={open} onClick={() => setOpen(!open)}>
        {open ? "Hide log" : `Log (${lines.length})`}
      </button>
      {open && (
        <pre className="log-lines plugin-log">
          {lines.map((l, i) => (
            <div key={i} className={l.level === "info" ? "" : "bad-text"}>
              {new Date(l.time * 1000).toLocaleTimeString()} {l.text}
            </div>
          ))}
        </pre>
      )}
    </>
  );
}

function AddPlugin() {
  const [open, setOpen] = useState(false);
  const [path, setPath] = useState("");
  if (!open)
    return (
      <button className="btn ghost" onClick={() => setOpen(true)}>
        Add from a file…
      </button>
    );
  return (
    <form
      className="add-plugin"
      onSubmit={(e) => {
        e.preventDefault();
        if (path.trim()) addPlugin(path.trim());
        setPath("");
        setOpen(false);
      }}
    >
      <div className="field wide">
        <label className="field-label" htmlFor="add-plugin-path">
          Plugin executable, on the recorder's computer
        </label>
        <input id="add-plugin-path" className="mono" autoFocus value={path} placeholder="/path/to/my-plugin" onChange={(e) => setPath(e.target.value)} />
        <span className="field-hint">A plugin you built, or unpacked from a release. It's asked who it is before it's added; it stays off until you turn it on.</span>
      </div>
      <div className="row">
        <button className="btn primary" type="submit" disabled={!path.trim()}>
          Add
        </button>
        <button className="btn ghost" type="button" onClick={() => setOpen(false)}>
          Cancel
        </button>
      </div>
    </form>
  );
}

/** "3 minutes ago", for when the list was fetched. */
function ago(t: number): string {
  const s = Math.max(0, Date.now() / 1000 - t);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  return new Date(t * 1000).toLocaleDateString();
}

function StoreSource(props: { store: PluginStore }) {
  const st = props.store;
  if (st.source === "registry") return <p className="muted small">From the plugin registry, checked {st.fetched ? ago(st.fetched) : "now"}.</p>;
  const when = st.source === "saved" && st.fetched ? `the list from ${new Date(st.fetched * 1000).toLocaleString()}` : "the list that came with this version";
  return (
    <p className="small warn-text" title={st.problem ?? undefined}>
      Couldn't reach the plugin registry, so this is {when}.
    </p>
  );
}

/** One of the registry's plugins: what it is, and Install / Update / Installed. */
function StoreCard(props: { l: StoreListing; installed?: PluginInfo; install?: PluginInstall; recording: boolean }) {
  const { l, installed, install } = props;
  const have = installed?.manifest?.version;
  let action: React.ReactNode;
  if (install) {
    action = (
      <span className="small muted install-stage" role="status">
        {STAGES[install.stage]}
      </span>
    );
  } else if (installed?.custom) {
    action = <span className="small muted">Your build is installed</span>;
  } else if (l.unavailable) {
    action = <span className="small muted">{l.unavailable}</span>;
  } else if (installed && have && !installed.unlistedFrom && !newer(l.version, have)) {
    action = <span className="chip ok">Installed{have !== l.version ? ` v${have}` : ""}</span>;
  } else if (installed && have) {
    action = (
      <button className="btn primary" onClick={() => installPlugin(l.id)}>
        {installed.unlistedFrom ? `Install the registry's v${l.version}` : `Update to v${l.version}`}
      </button>
    );
  } else {
    action = (
      <button className="btn primary" onClick={() => installPlugin(l.id)}>
        Install
      </button>
    );
  }
  return (
    <article className="plugin-card store-card" aria-labelledby={`sn-${l.id}`}>
      <div className="plugin-head">
        <span className="plugin-icon tone-off" aria-hidden="true">
          <IconPuzzle />
        </span>
        <div className="plugin-title">
          <div className="row">
            <strong id={`sn-${l.id}`}>{l.name}</strong>
            <span className="chip">v{l.version}</span>
            <TierChip tier={l.tier} />
          </div>
        </div>
        {action}
      </div>
      <p className="plugin-desc">{l.description}</p>
      <p className="small muted plugin-meta">
        {l.license && `${l.license} · `}
        {l.homepage && (
          <>
            <a href={l.homepage} target="_blank" rel="noreferrer">
              Website
            </a>
            {" · "}
          </>
        )}
        <a href={l.repository} target="_blank" rel="noreferrer">
          Source
        </a>
      </p>
    </article>
  );
}

/** A plugin from a GitHub release that isn't in the registry. */
function InstallFromGitHub(props: { busy: boolean }) {
  const [open, setOpen] = useState(false);
  const [repo, setRepo] = useState("");
  const [tag, setTag] = useState("");
  if (!open)
    return (
      <button className="btn ghost" onClick={() => setOpen(true)}>
        Install from GitHub…
      </button>
    );
  return (
    <form
      className="add-plugin"
      onSubmit={(e) => {
        e.preventDefault();
        if (!repo.trim()) return;
        installPluginFrom(repo.trim(), tag);
        setRepo("");
        setTag("");
        setOpen(false);
      }}
    >
      <p className="small warn-text">
        Plugins from outside the registry haven't been reviewed by anyone. A plugin runs with the same access to this computer as the recorder, so install
        only ones you trust.
      </p>
      <div className="grid2">
        <div className="field">
          <label className="field-label" htmlFor="gh-repo">
            GitHub repository, or a release's page
          </label>
          <input id="gh-repo" className="mono" autoFocus value={repo} placeholder="https://github.com/someone/trunk-plugin-pager" onChange={(e) => setRepo(e.target.value)} />
        </div>
        <div className="field">
          <label className="field-label" htmlFor="gh-tag">
            Release (optional)
          </label>
          <input id="gh-tag" className="mono" value={tag} placeholder="the latest" onChange={(e) => setTag(e.target.value)} />
        </div>
      </div>
      <span className="field-hint">It has to be released with the plugin template's release workflow. It's checked against its release's checksums, and stays off until you turn it on.</span>
      <div className="row">
        <button className="btn primary" type="submit" disabled={!repo.trim() || props.busy}>
          Install
        </button>
        <button className="btn ghost" type="button" onClick={() => setOpen(false)}>
          Cancel
        </button>
      </div>
    </form>
  );
}

function PluginStorePanel(props: { list: PluginsList; recording: boolean }) {
  const s = useApp();
  const store = s.pluginStore;
  const [query, setQuery] = useState("");
  useEffect(() => {
    if (!store) fetchPluginStore();
  }, [store]);
  const q = query.trim().toLowerCase();
  const order = { official: 0, community: 1, unlisted: 2 };
  const listings = (store?.plugins ?? [])
    .filter((l) => !q || [l.id, l.name, l.description].some((f) => f.toLowerCase().includes(q)))
    .sort((a, b) => order[a.tier] - order[b.tier] || a.name.localeCompare(b.name));
  // Installs from GitHub, by repository: shown until they're done.
  const fromGitHub = Object.values(s.pluginInstalls).filter((i) => !store?.plugins.some((l) => l.id === i.key));

  return (
    <section className="panel" aria-labelledby="store-title">
      <header className="panel-head">
        <h2 id="store-title">Find plugins</h2>
        <button className="btn ghost small" onClick={() => fetchPluginStore(true)} disabled={!store}>
          Check again
        </button>
      </header>
      {!store ? (
        <p className="muted">Asking the plugin registry…</p>
      ) : (
        <div className="stack">
          <StoreSource store={store} />
          {store.plugins.length > 4 && (
            <div className="field">
              <label className="field-label" htmlFor="store-search">
                Search
              </label>
              <input id="store-search" type="search" value={query} placeholder="OpenMHz, upload, stream…" onChange={(e) => setQuery(e.target.value)} />
            </div>
          )}
          {listings.map((l) => (
            <StoreCard key={l.id} l={l} installed={props.list.plugins.find((p) => p.id === l.id)} install={s.pluginInstalls[l.id]} recording={props.recording} />
          ))}
          {listings.length === 0 && <p className="empty">{q ? `No plugins match “${query.trim()}”.` : "The registry has no plugins yet."}</p>}
          {fromGitHub.map((i) => (
            <p key={i.key} className="small muted" role="status">
              {i.key}: {STAGES[i.stage]}
            </p>
          ))}
          <div className="row">
            <InstallFromGitHub busy={fromGitHub.length > 0} />
          </div>
        </div>
      )}
    </section>
  );
}

export function PluginsPage() {
  const s: AppState = useApp();
  const list = s.plugins;
  const recording = s.phase === "running" || s.phase === "starting";
  if (!list) return <p className="muted">Asking the recorder about plugins…</p>;
  const on = (p: PluginInfo) => Number(pluginOn(s.config, p.id));
  const plugins = [...list.plugins].sort((a, b) => on(b) - on(a) || (a.manifest?.name ?? a.id).localeCompare(b.manifest?.name ?? b.id));

  return (
    <div className="plugins-page">
      <section className="panel" aria-labelledby="plugins-title">
        <header className="panel-head">
          <h2 id="plugins-title">Plugins</h2>
          <AddPlugin />
        </header>
        <p className="muted small plugins-intro">
          Plugins are programs the recorder runs while it records and tells what happens: they upload calls to services like OpenMHz, stream audio, or send
          alerts. They only listen — nothing they do changes what's recorded. Install them here; set them up in Setup, where each system's card has its
          settings for each plugin.
        </p>
        <div className="stack">
          {plugins.map((p) => (
            <PluginCard
              key={p.id}
              p={p}
              recording={recording}
              listing={s.pluginStore?.plugins.find((l) => l.id === p.id)}
              install={s.pluginInstalls[p.id]}
            />
          ))}
          {plugins.length === 0 && <p className="empty">No plugins yet. Find one below, or add one you've built with <b>Add from a file</b>.</p>}
        </div>
      </section>
      <PluginStorePanel list={list} recording={recording} />
    </div>
  );
}
