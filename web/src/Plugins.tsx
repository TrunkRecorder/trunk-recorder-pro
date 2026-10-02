// The Plugins page (desktop app): the plugins the recorder knows, each with
// an on/off switch, how it's doing while recording, and a settings form drawn
// from the schema the plugin describes itself with (trunk-recorder-plugin's
// schema.rs lists the subset). Settings are saved to plugins.json on the
// recorder; while recording, plugins restart with them at once.

import { useEffect, useState } from "react";
import { addPlugin, removePlugin, savePluginSettings, setPluginAudio, setPluginEnabled, useApp, type AppState } from "./controller.ts";
import { openTodos } from "./todo.ts";
import type { PluginInfo, PluginSchema, PluginValues, PluginsList } from "./protocol.ts";

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

function isUrl(s: string): boolean {
  try {
    const u = new URL(s);
    return u.protocol === "http:" || u.protocol === "https:";
  } catch {
    return false;
  }
}

/** One setting. Empty text means "not set": the plugin's default applies (shown as the placeholder). */
function SchemaField(props: { name: string; schema: PluginSchema; value: unknown; onChange: (v: unknown) => void; id: string }) {
  const { schema: f, value, onChange, id } = props;
  const label = f.title ?? spell(props.name);
  const hint = f.description;
  const [shown, setShown] = useState(false);

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
    const on = typeof value === "boolean" ? value : f.default === true;
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
  if (f.enum) {
    const labels = f["x-enum-labels"];
    const cur = value ?? f.default ?? f.enum[0];
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
        placeholder={f.default !== undefined && f.default !== null ? String(f.default) : ""}
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
        placeholder={Array.isArray(f.default) && f.default.length ? f.default.join(", ") : "Separate with commas"}
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
    const placeholder = typeof f.default === "string" ? f.default : "";
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
      {problem ? <span className="field-hint bad-text">{problem}</span> : hint && <span className="field-hint">{hint}</span>}
    </div>
  );
}

function SchemaFields(props: { schema: PluginSchema | undefined; values: PluginValues; onChange: (v: PluginValues) => void; id: string }) {
  return (
    <div className="stack plugin-fields">
      {fieldsOf(props.schema).map(([k, f]) => (
        <SchemaField
          key={k}
          name={k}
          schema={f}
          id={`${props.id}-${k}`}
          value={props.values[k]}
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

function PluginSettings(props: { p: PluginInfo; systems: string[]; recording: boolean; onClose: () => void }) {
  const { p } = props;
  const m = p.manifest!;
  const [config, setConfig] = useState<PluginValues>(p.config ?? {});
  const [systems, setSystems] = useState<Record<string, PluginValues>>(p.systems ?? {});
  const dirty = JSON.stringify(config) !== JSON.stringify(p.config ?? {}) || JSON.stringify(systems) !== JSON.stringify(p.systems ?? {});
  // Systems that are gone keep their settings (they may come back), but aren't shown.
  const perSystem = hasFields(m.system_config);

  return (
    <div className="plugin-settings stack">
      {hasFields(m.config) && <SchemaFields schema={m.config} values={config} onChange={setConfig} id={`p-${p.id}`} />}
      {perSystem && (
        <div className="stack">
          <h4>For each system</h4>
          {props.systems.length === 0 && <p className="empty small">No systems yet: add one on the Recorder page.</p>}
          {props.systems.map((name) => (
            <fieldset className="plugin-group" key={name}>
              <legend>{name}</legend>
              <SchemaFields
                schema={m.system_config}
                values={systems[name] ?? {}}
                onChange={(v) => setSystems({ ...systems, [name]: v })}
                id={`p-${p.id}-${name}`}
              />
            </fieldset>
          ))}
        </div>
      )}
      <div className="row">
        <button
          className="btn primary"
          disabled={!dirty}
          onClick={() => {
            savePluginSettings(p.id, config, systems);
            props.onClose();
          }}
        >
          Save
        </button>
        <button className="btn ghost" onClick={props.onClose}>
          {dirty ? "Cancel" : "Close"}
        </button>
        {dirty && p.enabled && <span className="muted small">{props.recording ? "Plugins restart with the new settings." : "Used the next time recording starts."}</span>}
      </div>
    </div>
  );
}

/** How a plugin is doing, in a word or two, and its tone. */
function stateOf(p: PluginInfo, recording: boolean): { tone: "ok" | "warn" | "bad" | "off"; text: string } {
  if (p.problem) return { tone: "bad", text: "Can't run" };
  if (!p.enabled) return { tone: "off", text: "Off" };
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

function PluginCard(props: { p: PluginInfo; systems: string[]; recording: boolean }) {
  const { p, recording } = props;
  const m = p.manifest;
  const [editing, setEditing] = useState(false);
  const st = stateOf(p, recording);
  const r = p.runtime;
  const configurable = !!m && (hasFields(m.config) || hasFields(m.system_config));
  const message = p.problem ?? (p.enabled && r.message && r.message !== "running" ? r.message : "");
  const counted = r.ok + r.skipped + r.failed > 0;
  const needsM4a = m?.audio_formats.includes("m4a");
  // Its settings came over from Trunk Recorder, and it isn't running yet.
  const imported = openTodos(useApp()).find((t) => t.target === `plugin-${p.id}`);

  return (
    <article className={`plugin-card${p.enabled ? "" : " off"}${imported ? " needs" : ""}`} aria-labelledby={`pn-${p.id}`} id={`need-plugin-${p.id}`}>
      {imported && <div className="field-needs">{imported.text}</div>}
      <div className="row plugin-head">
        <span className={`dot dot-${st.tone}`} aria-hidden="true" />
        <strong id={`pn-${p.id}`}>{m?.name ?? p.id}</strong>
        {m && <span className="chip">v{m.version}</span>}
        {p.custom && (
          <span className="chip" title={p.path}>
            your build
          </span>
        )}
        <span className={`plugin-state tone-${st.tone}`}>{st.text}</span>
        <span className="spacer" />
        <label className="toggle" title={p.problem ? "It can't run: see below" : ""}>
          <input type="checkbox" checked={p.enabled} disabled={!!p.problem && !p.enabled} onChange={(e) => setPluginEnabled(p.id, e.target.checked)} />
          <span>On</span>
        </label>
      </div>
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

      {editing && m ? (
        <PluginSettings p={p} systems={props.systems} recording={recording} onClose={() => setEditing(false)} />
      ) : (
        <div className="row plugin-actions">
          {configurable && (
            <button className="btn" onClick={() => setEditing(true)}>
              Settings
            </button>
          )}
          {r.log.length > 0 && <PluginLog p={p} />}
          <span className="spacer" />
          <button
            className="btn ghost small danger"
            onClick={() => {
              const what = p.custom ? `Remove ${m?.name ?? p.id} from the recorder? Your build at ${p.path} isn't touched.` : `Uninstall ${m?.name ?? p.id}? Its settings are forgotten.`;
              if (confirm(what)) removePlugin(p.id);
            }}
          >
            {p.custom ? "Remove" : "Uninstall"}
          </button>
        </div>
      )}
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

function AudioSettings(props: { list: PluginsList }) {
  const a = props.list.audio;
  const wanted = props.list.plugins.filter((p) => p.enabled && p.manifest?.audio_formats.includes("m4a"));
  const [kbps, setKbps] = useState(String(a.bitrateKbps));
  useEffect(() => setKbps(String(a.bitrateKbps)), [a.bitrateKbps]);
  return (
    <section className="panel" aria-labelledby="plugin-audio">
      <header className="panel-head">
        <h2 id="plugin-audio">M4A audio</h2>
        {a.found ? <span className="chip ok">{a.found}</span> : <span className={`chip ${wanted.length ? "bad" : ""}`}>{a.encoder === "none" ? "off" : "no encoder found"}</span>}
      </header>
      <div className="stack">
        <p className="muted small">
          Some plugins upload calls as M4A, which is about a tenth the size of WAV. The recorder encodes each call once for all of them, with an encoder already on this
          computer.{" "}
          {wanted.length > 0 && !a.found && (
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
            <select id="m4a-encoder" value={a.encoder} onChange={(e) => setPluginAudio({ encoder: e.target.value })}>
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
                if (k >= 8 && k <= 320 && k !== a.bitrateKbps) setPluginAudio({ bitrateKbps: k });
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

export function PluginsPage() {
  const s: AppState = useApp();
  const list = s.plugins;
  const recording = s.phase === "running" || s.phase === "starting";
  if (!list) return <p className="muted">Asking the recorder about plugins…</p>;
  const plugins = [...list.plugins].sort((a, b) => Number(b.enabled) - Number(a.enabled) || (a.manifest?.name ?? a.id).localeCompare(b.manifest?.name ?? b.id));

  return (
    <div className="plugins-page">
      <section className="panel" aria-labelledby="plugins-title">
        <header className="panel-head">
          <h2 id="plugins-title">Plugins</h2>
          <AddPlugin />
        </header>
        <p className="muted small plugins-intro">
          Plugins are programs the recorder runs while it records and tells what happens: they upload calls to services like OpenMHz, stream audio, or send
          alerts. They only listen — nothing they do changes what's recorded.
        </p>
        {list.problem && <p className="bad-text">{list.problem}</p>}
        <div className="stack">
          {plugins.map((p) => (
            <PluginCard key={p.id} p={p} systems={list.systems} recording={recording} />
          ))}
          {plugins.length === 0 && (
            <p className="empty">
              No plugins yet. A plugin store is on its way; for now, add a plugin you've built or downloaded with <b>Add from a file</b>.
            </p>
          )}
        </div>
        <p className="muted small plugins-file">
          Settings are kept in <span className="mono">{list.file}</span>.
        </p>
      </section>
      <AudioSettings list={list} />
    </div>
  );
}
