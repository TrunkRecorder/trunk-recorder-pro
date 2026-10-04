// Setup → Recording → Interfaces (desktop app): interfaces of the user's own
// (folders the recorder serves at /ui/<name>/, docs/api), which one / shows,
// and the web pages elsewhere allowed to use the API.

import { useEffect, useState } from "react";
import { updateConfig, useApp } from "./controller.ts";
import type { CustomInterface, InterfacesInfo } from "./protocol.ts";

const NAME = /^[A-Za-z0-9_-]{1,64}$/;

export function InterfacesPanel() {
  const c = useApp().config;
  const server = c?.server;
  const list = server?.interfaces ?? [];
  const [info, setInfo] = useState<InterfacesInfo | null>(null);
  const [origins, setOrigins] = useState((server?.allowedOrigins ?? []).join("\n"));
  useEffect(() => setOrigins((server?.allowedOrigins ?? []).join("\n")), [JSON.stringify(server?.allowedOrigins)]);
  // How each folder is doing, from the recorder (after the config is saved).
  useEffect(() => {
    const t = setTimeout(() => {
      fetch("/api/interfaces")
        .then((r) => r.json())
        .then(setInfo)
        .catch(() => setInfo(null));
    }, 600);
    return () => clearTimeout(t);
  }, [JSON.stringify(list)]);
  if (!server) return null;

  const edit = (k: number, patch: Partial<CustomInterface>) =>
    updateConfig((x) => {
      const l = (x.server.interfaces ??= []);
      if (patch.name !== undefined && x.server.home === l[k].name) x.server.home = patch.name;
      l[k] = { ...l[k], ...patch };
    });
  const remove = (k: number) =>
    updateConfig((x) => {
      const l = (x.server.interfaces ??= []);
      if (x.server.home === l[k].name) x.server.home = "";
      l.splice(k, 1);
    });
  const add = () =>
    updateConfig((x) => {
      const l = (x.server.interfaces ??= []);
      let n = l.length + 1;
      while (l.some((i) => i.name === `interface-${n}`)) n++;
      l.push({ name: `interface-${n}`, path: "" });
    });
  const problem = (i: CustomInterface, k: number): string | null => {
    if (!NAME.test(i.name)) return "Name: letters, digits, - and _ only.";
    if (list.findIndex((x) => x.name === i.name) !== k) return "Name already used.";
    if (!i.path.trim()) return "Folder needed.";
    return info?.interfaces.find((x) => x.name === i.name && x.path === i.path)?.problem ?? null;
  };
  const home = server.home ?? "";

  return (
    <section className="panel" aria-labelledby="ui-title">
      <header className="panel-head">
        <h2 id="ui-title">Interfaces</h2>
        <a className="btn small" href="/ui/" target="_blank" rel="noreferrer">
          All interfaces
        </a>
      </header>
      <div className="stack">
        <p className="muted small">
          Folders with an <code>index.html</code>, served at <code>/ui/&lt;name&gt;/</code>.{" "}
          <a href="/api/docs" target="_blank" rel="noreferrer">API</a> · <a href="/api/llms.txt" target="_blank" rel="noreferrer">llms.txt</a> ·{" "}
          <a href="/ui/" target="_blank" rel="noreferrer">Examples</a>
        </p>
        {list.map((i, k) => {
          const p = problem(i, k);
          return (
            <div key={k} className="row wrap" style={{ alignItems: "flex-start", gap: 8 }}>
              <div className="field" style={{ width: "12em" }}>
                <label className="field-label" htmlFor={`ui-name-${k}`}>
                  Name
                </label>
                <input id={`ui-name-${k}`} className="mono" value={i.name} onChange={(e) => edit(k, { name: e.target.value.trim() })} />
              </div>
              <div className="field" style={{ flex: 1, minWidth: "16em" }}>
                <label className="field-label" htmlFor={`ui-path-${k}`}>
                  Folder
                </label>
                <input id={`ui-path-${k}`} className="mono" value={i.path} placeholder="/home/me/scanner-page" onChange={(e) => edit(k, { path: e.target.value })} />
                <span className={`field-hint ${p ? "bad-text" : ""}`}>
                  {p ?? (
                    <a href={`/ui/${encodeURIComponent(i.name)}/`} target="_blank" rel="noreferrer">
                      Open /ui/{i.name}/
                    </a>
                  )}
                </span>
              </div>
              <button className="btn small danger" style={{ marginTop: 22 }} onClick={() => remove(k)} aria-label={`Remove ${i.name}`}>
                Remove
              </button>
            </div>
          );
        })}
        <div className="row wrap" style={{ gap: 12, alignItems: "flex-end" }}>
          <button className="btn" onClick={add}>
            Add an interface
          </button>
          <div className="field">
            <label className="field-label" htmlFor="ui-home">
              Show at /
            </label>
            <select id="ui-home" value={home} onChange={(e) => updateConfig((x) => void (x.server.home = e.target.value))}>
              <option value="">This interface</option>
              {list.map((i) => (
                <option key={i.name} value={i.name}>
                  {i.name}
                </option>
              ))}
            </select>
            <span className="field-hint">Built-in is always at /builtin/{info && typeof info.home === "object" ? ` (--ui ${info.home.folder} is at / now)` : ""}</span>
          </div>
        </div>
        <div className="field">
          <label className="field-label" htmlFor="ui-origins">
            Other pages allowed to use the API
          </label>
          <textarea
            id="ui-origins"
            className="mono"
            rows={2}
            value={origins}
            placeholder={"http://192.168.1.50:3000\nnull"}
            onChange={(e) => setOrigins(e.target.value)}
            onBlur={() =>
              updateConfig(
                (x) =>
                  void (x.server.allowedOrigins = origins
                    .split(/[\s,]+/)
                    .map((o) => o.trim())
                    .filter(Boolean)),
              )
            }
          />
          <span className="field-hint">
            One origin per line (scheme://host:port). <code>null</code>: local files; <code>*</code>: any page — private networks only.
          </span>
        </div>
      </div>
    </section>
  );
}
