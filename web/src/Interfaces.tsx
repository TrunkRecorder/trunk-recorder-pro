// Interfaces (desktop app): pages besides this one, for people who listen
// rather than run the recorder. Each is served on a port of its own, a site
// of its own to the browser, with the recorder's API beside it and the
// access the config gives it (docs/interfaces). This page lists them (open,
// on or off, access, update, remove), sets their ports, installs them from
// the interface store, adds folders of one's own, and lists the pages
// elsewhere allowed to use this interface's API. All of it is in the
// config's `server`, saved as it's changed; the recorder serves the changes
// at once (this interface's own port takes a restart).

import { useEffect, useState } from "react";
import { addInterface, fetchInterfaceStore, installInterface, installInterfaceFrom, removeInterface, setInterface, updateConfig, useApp } from "./controller.ts";
import { Switch } from "./SetupLayout.tsx";
import { Author, newer, Screenshots, STAGES, StoreSource, TierChip, webUrl } from "./StoreParts.tsx";
import type { ApiAccess, Config, InterfaceInfo, InterfaceListing, PluginInstall } from "./protocol.ts";

/** Where interface `port` is, from this browser. */
const urlOf = (port: number) => `${location.protocol}//${location.hostname}:${port}/`;

const CONTROL_WARNING =
  "With control it can do anything this interface can: change settings, start and stop recording, install plugins (programs that run on this computer) and quit the recorder. Give control only to interfaces you trust.";

function IconWindow() {
  return (
    <svg viewBox="0 0 20 20" fill="none" stroke="currentColor" strokeWidth="1.6">
      <rect x="2.5" y="3.5" width="15" height="13" rx="2" />
      <path d="M2.5 7.5h15M5 5.5h.01M7 5.5h.01" />
    </svg>
  );
}

/** Why a port can't be used for interface `id` (or the built-in one, ""): the config's, or --ui's this run. */
function portProblem(c: Config, id: string, port: number | undefined, list: InterfaceInfo[]): string | null {
  if (!port || port < 1 || port > 65535 || !Number.isInteger(port)) return "A port is a number from 1 to 65535.";
  const here = Number(location.port);
  if (id && (port === c.server.port || port === here)) return "That's this interface's port.";
  const other = Object.entries(c.server.interfaces ?? {}).find(([o, i]) => o !== id && i.port === port);
  if (other) return `That's ${other[0]}'s port.`;
  const dev = list.find((i) => i.dev && i.port === port);
  return dev ? `That's ${dev.id}'s port this run (--ui).` : null;
}

/** A port, saved when it's left (or Enter): each keystroke would move the interface. */
function PortInput(props: { id: string; value: number; c: Config; list: InterfaceInfo[]; onSave: (p: number) => void; label: string }) {
  const [text, setText] = useState(String(props.value));
  useEffect(() => setText(String(props.value)), [props.value]);
  const n = Number(text);
  const problem = text === String(props.value) ? null : portProblem(props.c, props.id, n, props.list);
  const save = () => {
    if (!problem && n !== props.value) props.onSave(n);
    else setText(String(props.value));
  };
  return (
    <span className="iface-port">
      <input
        className="mono narrow"
        inputMode="numeric"
        aria-label={props.label}
        aria-invalid={!!problem}
        value={text}
        onChange={(e) => setText(e.target.value.trim())}
        onBlur={save}
        onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
      />
      {problem && <span className="small bad-text">{problem}</span>}
    </span>
  );
}

function stateOf(i: InterfaceInfo, on: boolean): { tone: string; text: string } {
  if (!on) return { tone: "off", text: "Off" };
  if (i.problem) return { tone: "bad", text: "Not served" };
  if (i.serving) return { tone: "ok", text: `Serving on port ${i.port}` };
  return { tone: "warn", text: "Starting…" };
}

/** One of this recorder's interfaces: what it is, how it's doing, and what it may do. */
function InterfaceCard(props: { i: InterfaceInfo; listing?: InterfaceListing; install?: PluginInstall }) {
  const { i, listing, install } = props;
  const c = useApp().config;
  const setup = c?.server.interfaces?.[i.id];
  const on = i.dev || !!setup?.enabled;
  const m = i.manifest;
  const name = m?.name ?? i.id;
  const st = stateOf(i, on);
  const update = listing && m && i.installed && i.reviewed && !listing.unavailable && newer(listing.version, m.version) ? listing : null;
  const setAccess = (a: ApiAccess) => {
    if (a === "control" && !confirm(`Give ${name} control?\n\n${CONTROL_WARNING}`)) return;
    setInterface(i.id, { access: a });
  };
  return (
    <article className={`plugin-card${on ? "" : " off"}`} aria-labelledby={`if-${i.id}`}>
      <div className="plugin-head">
        <span className={`plugin-icon tone-${st.tone}`} aria-hidden="true">
          <IconWindow />
        </span>
        <div className="plugin-title">
          <div className="row">
            <strong id={`if-${i.id}`}>{name}</strong>
            {m && <span className="chip">v{m.version}</span>}
            {i.dev ? (
              <span className="chip" title={i.folder}>
                this run only
              </span>
            ) : !i.installed ? (
              <span className="chip" title={i.folder}>
                your folder
              </span>
            ) : i.reviewed ? (
              listing && <TierChip tier={listing.tier} />
            ) : (
              <TierChip tier="unreviewed" title={i.repository ? `From ${i.repository}; unreviewed` : undefined} />
            )}
            {update && <span className="chip warn">v{update.version} available</span>}
            <Author repository={i.repository ?? m?.repository} />
          </div>
          <span className={`plugin-state tone-${st.tone}`}>{st.text}</span>
        </div>
        {!i.dev && setup && <Switch label={on ? "On" : "Off"} checked={on} onChange={(v) => setInterface(i.id, { enabled: v })} />}
      </div>
      {m?.description && <p className="plugin-desc">{m.description}</p>}
      <Screenshots name={name} shots={(m?.screenshots ?? []).map((sh, k) => ({ src: `/interfaces/${encodeURIComponent(i.id)}/screenshots/${k}`, caption: sh.caption }))} />
      {on && i.problem && <p className="small bad-text">{i.problem}</p>}
      <p className="small muted plugin-meta">
        {i.access === "control" ? <span className="warn-text">Control: it can change anything</span> : "Listen: it sees and plays, and changes nothing"}
        {m?.access === "control" && i.access === "listen" && " (it asks for control)"}
        {" · "}
        {i.installed ? (
          webUrl(m?.homepage || i.repository || m?.repository) && (
            <a href={webUrl(m?.homepage || i.repository || m?.repository)} target="_blank" rel="noreferrer">
              {m?.homepage ? "Website" : "Source"}
            </a>
          )
        ) : (
          <span className="mono">{i.folder}</span>
        )}
      </p>
      <div className="row plugin-actions">
        {on && i.serving && (
          <a className="btn primary" href={urlOf(i.port)} target="_blank" rel="noreferrer">
            Open
          </a>
        )}
        {!i.dev && setup && (
          <label className="iface-access">
            <span className="small muted">Access</span>
            <select value={setup.access} onChange={(e) => setAccess(e.target.value as ApiAccess)}>
              <option value="listen">Listen</option>
              <option value="control">Control</option>
            </select>
          </label>
        )}
        {install ? (
          <span className="small muted install-stage" role="status">
            {STAGES[install.stage]}
          </span>
        ) : (
          update && (
            <button className="btn" onClick={() => installInterface(i.id)}>
              Update to v{update.version}
            </button>
          )
        )}
        <span className="spacer" />
        {!i.dev && (
          <button
            className="btn ghost small danger"
            disabled={!!install}
            onClick={() => {
              const what = i.installed ? `Uninstall ${name}? Its pages stop being served.` : `Stop serving ${name}? ${i.folder} stays.`;
              if (confirm(what)) removeInterface(i.id);
            }}
          >
            {i.installed ? "Uninstall" : "Remove"}
          </button>
        )}
      </div>
    </article>
  );
}

function AddFolder() {
  const [open, setOpen] = useState(false);
  const [path, setPath] = useState("");
  if (!open)
    return (
      <button className="btn ghost" onClick={() => setOpen(true)}>
        Add a folder…
      </button>
    );
  return (
    <form
      className="add-plugin"
      onSubmit={(e) => {
        e.preventDefault();
        if (path.trim()) addInterface(path.trim());
        setPath("");
        setOpen(false);
      }}
    >
      <div className="field wide">
        <label className="field-label" htmlFor="add-iface-path">
          A folder on the recorder's computer with an index.html
        </label>
        <input id="add-iface-path" className="mono" autoFocus value={path} placeholder="~/scanner-page" onChange={(e) => setPath(e.target.value)} />
        <span className="field-hint">
          Its <code>interface.json</code>, if it has one, names it and says what access it needs. Edits show when its page is reloaded.
        </span>
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

/** The ports: this interface's and every other's. */
function PortsPanel(props: { c: Config; list: InterfaceInfo[] }) {
  const { c, list } = props;
  const here = Number(location.port) || (location.protocol === "https:" ? 443 : 80);
  return (
    <section className="panel" aria-labelledby="ports-title">
      <header className="panel-head">
        <h2 id="ports-title">Ports</h2>
      </header>
      <p className="muted small plugins-intro">
        Each interface is its own site, at this computer's address and its port. A change moves it at once; open pages reconnect at the new port.
      </p>
      <div className="iface-ports" role="table">
        <div className="iface-ports-row" role="row">
          <span role="cell">
            <strong>Trunk Recorder Pro</strong> <span className="muted small">this interface</span>
          </span>
          <span role="cell">
            <PortInput id="" value={c.server.port} c={c} list={list} label="This interface's port" onSave={(p) => updateConfig((x) => void (x.server.port = p))} />
          </span>
          <span role="cell" className="small muted">
            {c.server.port !== here && location.port ? `Takes effect when the recorder restarts (now ${here})` : "Changes take effect when the recorder restarts"}
          </span>
        </div>
        {list
          .filter((i) => !i.dev)
          .map((i) => {
            const setup = c.server.interfaces?.[i.id];
            if (!setup) return null;
            return (
              <div key={i.id} className="iface-ports-row" role="row">
                <span role="cell">
                  <strong>{i.manifest?.name ?? i.id}</strong>
                </span>
                <span role="cell">
                  <PortInput id={i.id} value={setup.port} c={c} list={list} label={`${i.manifest?.name ?? i.id}'s port`} onSave={(p) => setInterface(i.id, { port: p })} />
                </span>
                <span role="cell" className="small">
                  {!setup.enabled ? (
                    <span className="muted">Off</span>
                  ) : i.serving ? (
                    <a href={urlOf(i.port)} target="_blank" rel="noreferrer">
                      {urlOf(i.port)}
                    </a>
                  ) : (
                    <span className="bad-text">{i.problem ?? "Not served"}</span>
                  )}
                </span>
              </div>
            );
          })}
      </div>
      <p className="muted small">
        Other computers reach them only when the recorder listens on every address (<code>server.bind</code> <code>0.0.0.0</code>, now{" "}
        <code>{c.server.bind}</code>), and their ports are open in the firewall.
      </p>
    </section>
  );
}

/** One of the store's interfaces: what it is, and Install / Update / Installed. */
function StoreCard(props: { l: InterfaceListing; installed?: InterfaceInfo; install?: PluginInstall }) {
  const { l, installed, install } = props;
  const have = installed?.manifest?.version;
  const go = () => {
    if (!installed && l.access === "control" && !confirm(`${l.name} asks for control.\n\n${CONTROL_WARNING}`)) return;
    installInterface(l.id);
  };
  let action: React.ReactNode;
  if (install) {
    action = (
      <span className="small muted install-stage" role="status">
        {STAGES[install.stage]}
      </span>
    );
  } else if (installed && !installed.installed) {
    action = <span className="small muted">Your folder is {l.id}</span>;
  } else if (l.unavailable) {
    action = <span className="small muted">{l.unavailable}</span>;
  } else if (installed && have && installed.reviewed && !newer(l.version, have)) {
    action = <span className="chip ok">Installed{have !== l.version ? ` v${have}` : ""}</span>;
  } else {
    action = (
      <button className="btn primary" onClick={go}>
        {installed && have ? (installed.reviewed ? `Update to v${l.version}` : `Install the store's v${l.version}`) : "Install"}
      </button>
    );
  }
  return (
    <article className="plugin-card store-card" aria-labelledby={`is-${l.id}`}>
      <div className="plugin-head">
        <span className="plugin-icon tone-off" aria-hidden="true">
          <IconWindow />
        </span>
        <div className="plugin-title">
          <div className="row">
            <strong id={`is-${l.id}`}>{l.name}</strong>
            <span className="chip">v{l.version}</span>
            <TierChip tier={l.tier} />
            <Author repository={l.repository} />
            {l.access === "control" && (
              <span className="chip warn" title={CONTROL_WARNING}>
                asks for control
              </span>
            )}
          </div>
        </div>
        {action}
      </div>
      <p className="plugin-desc">{l.description}</p>
      <Screenshots name={l.name} shots={(l.screenshots ?? []).map((sh) => ({ src: `/store/screenshots/${sh.sha256}`, caption: sh.caption }))} />
      <p className="small muted plugin-meta">
        {l.license && `${l.license} · `}
        {l.homepage && (
          <>
            <a href={webUrl(l.homepage)} target="_blank" rel="noreferrer">
              Website
            </a>
            {" · "}
          </>
        )}
        <a href={webUrl(l.repository)} target="_blank" rel="noreferrer">
          Source
        </a>
      </p>
    </article>
  );
}

/** An interface from a GitHub release that isn't in the store. */
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
        installInterfaceFrom(repo.trim(), tag);
        setRepo("");
        setTag("");
        setOpen(false);
      }}
    >
      <p className="small warn-text">Unreviewed. It may only listen until you give it control.</p>
      <div className="grid2">
        <div className="field">
          <label className="field-label" htmlFor="gh-iface-repo">
            GitHub repository or release URL
          </label>
          <input id="gh-iface-repo" className="mono" autoFocus value={repo} placeholder="https://github.com/someone/trunkamp" onChange={(e) => setRepo(e.target.value)} />
        </div>
        <div className="field">
          <label className="field-label" htmlFor="gh-iface-tag">
            Release (optional)
          </label>
          <input id="gh-iface-tag" className="mono" value={tag} placeholder="latest" onChange={(e) => setTag(e.target.value)} />
        </div>
      </div>
      <span className="field-hint">Must use the interface template's release workflow.</span>
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

function StorePanel(props: { list: InterfaceInfo[] }) {
  const s = useApp();
  const store = s.interfaceStore;
  const [query, setQuery] = useState("");
  useEffect(() => {
    if (!store) fetchInterfaceStore();
  }, [store]);
  const q = query.trim().toLowerCase();
  const order = { official: 0, community: 1, unlisted: 2 };
  const listings = (store?.interfaces ?? [])
    .filter((l) => !q || [l.id, l.name, l.description].some((f) => f.toLowerCase().includes(q)))
    .sort((a, b) => order[a.tier] - order[b.tier] || a.name.localeCompare(b.name));
  const fromGitHub = Object.values(s.interfaceInstalls).filter((i) => !store?.interfaces.some((l) => l.id === i.key));
  return (
    <section className="panel" aria-labelledby="istore-title">
      <header className="panel-head">
        <h2 id="istore-title">Find interfaces</h2>
        <button className="btn ghost small" onClick={() => fetchInterfaceStore(true)} disabled={!store}>
          Check again
        </button>
      </header>
      {!store ? (
        <p className="muted">Loading…</p>
      ) : (
        <div className="stack">
          <StoreSource store={store} />
          {store.interfaces.length > 4 && (
            <div className="field">
              <label className="field-label" htmlFor="istore-search">
                Search
              </label>
              <input id="istore-search" type="search" value={query} placeholder="player, wall, map…" onChange={(e) => setQuery(e.target.value)} />
            </div>
          )}
          {listings.map((l) => (
            <StoreCard key={l.id} l={l} installed={props.list.find((i) => i.id === l.id)} install={s.interfaceInstalls[l.id]} />
          ))}
          {listings.length === 0 && <p className="empty">{q ? `No interfaces match “${query.trim()}”.` : "The store has no interfaces yet."}</p>}
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

/** Pages served elsewhere that may use this interface's API (with control). */
function OriginsPanel(props: { c: Config }) {
  const saved = props.c.server.allowedOrigins ?? [];
  const [origins, setOrigins] = useState(saved.join("\n"));
  useEffect(() => setOrigins(saved.join("\n")), [JSON.stringify(saved)]);
  return (
    <section className="panel" aria-labelledby="origins-title">
      <header className="panel-head">
        <h2 id="origins-title">Pages served elsewhere</h2>
      </header>
      <div className="field">
        <label className="field-label" htmlFor="ui-origins">
          Pages allowed to use this interface's API, with control
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
          One origin per line (scheme://host:port), for a page served by something else, such as a dev server. <code>null</code>: a page opened from a
          file; <code>*</code>: any page — private networks only. An interface the recorder serves needs none of this.
        </span>
      </div>
    </section>
  );
}

export function InterfacesPage() {
  const s = useApp();
  const c = s.config;
  const list = s.interfaces?.interfaces;
  if (!c || !list) return <p className="muted">Loading…</p>;
  const order = (i: InterfaceInfo) => Number(!!(i.dev || c.server.interfaces?.[i.id]?.enabled));
  const sorted = [...list].sort((a, b) => order(b) - order(a) || (a.manifest?.name ?? a.id).localeCompare(b.manifest?.name ?? b.id));
  return (
    <div className="plugins-page">
      <section className="panel" aria-labelledby="ifaces-title">
        <header className="panel-head">
          <h2 id="ifaces-title">Interfaces</h2>
          <AddFolder />
        </header>
        <p className="muted small plugins-intro">
          Pages for people who listen, apart from this one: a player, a scanner wall. Each runs in the browser and is served on a port of its own.{" "}
          <a href="/api/docs" target="_blank" rel="noreferrer">
            Build your own
          </a>
        </p>
        <div className="stack">
          {sorted.map((i) => (
            <InterfaceCard key={i.id} i={i} listing={s.interfaceStore?.interfaces.find((l) => l.id === i.id)} install={s.interfaceInstalls[i.id]} />
          ))}
          {sorted.length === 0 && <p className="empty">No interfaces yet. Install one below, or add a folder of your own.</p>}
        </div>
      </section>
      <PortsPanel c={c} list={sorted} />
      <StorePanel list={list} />
      <OriginsPanel c={c} />
    </div>
  );
}
