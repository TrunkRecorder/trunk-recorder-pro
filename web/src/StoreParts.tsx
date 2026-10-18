// The plugin store and the interface store on screen: one panel (search, the
// listings, installs from GitHub), one card, one GitHub form, by kind; and
// what they share: version order, how an install is going, a listing's tier,
// and how fresh the list is. Each page says only what its own listing's
// button is (installed, an update, your own copy…).

import { useEffect, useState } from "react";
import { fetchStore, installFromGitHub, useApp } from "./controller.ts";
import type { InstallProgress, InterfaceListing, PluginListing, StoreKind, StoreListing } from "./protocol.ts";

/** A listing of store `K`. */
export type ListingOf<K extends StoreKind> = K extends "plugin" ? PluginListing : InterfaceListing;

/** `url` when it's a web address; undefined otherwise (a manifest's `javascript:` link, say). */
export function webUrl(url: string | null | undefined): string | undefined {
  return url && /^https?:\/\//i.test(url) ? url : undefined;
}

/** Who made it: the GitHub account its repository is in ("someone" for github.com/someone/x). */
export function authorOf(repository: string | null | undefined): string | null {
  const m = /^https?:\/\/github\.com\/([A-Za-z0-9-]+)\//i.exec(repository ?? "");
  return m ? m[1] : null;
}

/** "by someone", linked to their GitHub page; nothing when the repository isn't on GitHub. */
export function Author(props: { repository: string | null | undefined }) {
  const who = authorOf(props.repository);
  if (!who) return null;
  return (
    <span className="small muted">
      by{" "}
      <a href={`https://github.com/${who}`} target="_blank" rel="noreferrer">
        {who}
      </a>
    </span>
  );
}

/** Screenshots: a row of thumbnails; one opens large, with its caption, to step through. */
export function Screenshots(props: { shots: { src: string; caption?: string }[]; name: string }) {
  const [open, setOpen] = useState<number | null>(null);
  const shots = props.shots;
  if (!shots.length) return null;
  const at = open === null ? null : shots[open];
  const step = (d: number) => setOpen((k) => (k === null ? k : (k + d + shots.length) % shots.length));
  return (
    <>
      <div className="shots" role="list" aria-label={`Screenshots of ${props.name}`}>
        {shots.map((s, k) => (
          <button key={s.src} className="shot" role="listitem" onClick={() => setOpen(k)} title={s.caption || `Screenshot ${k + 1}`}>
            <img src={s.src} alt={s.caption || `${props.name}, screenshot ${k + 1}`} loading="lazy" />
          </button>
        ))}
      </div>
      {at && (
        <div
          className="shot-view"
          role="dialog"
          aria-modal="true"
          aria-label={`${props.name}, screenshot ${open! + 1} of ${shots.length}`}
          onClick={() => setOpen(null)}
          onKeyDown={(e) => (e.key === "Escape" ? setOpen(null) : e.key === "ArrowRight" ? step(1) : e.key === "ArrowLeft" ? step(-1) : undefined)}
          tabIndex={-1}
          ref={(el) => el?.focus()}
        >
          <figure onClick={(e) => e.stopPropagation()}>
            <img src={at.src} alt={at.caption || `${props.name}, screenshot ${open! + 1}`} />
            <figcaption className="row">
              {shots.length > 1 && (
                <button className="btn ghost small" onClick={() => step(-1)} aria-label="Previous">
                  ‹
                </button>
              )}
              <span>{at.caption || props.name}</span>
              <span className="spacer" />
              <span className="small muted">
                {open! + 1} / {shots.length}
              </span>
              {shots.length > 1 && (
                <button className="btn ghost small" onClick={() => step(1)} aria-label="Next">
                  ›
                </button>
              )}
              <button className="btn ghost small" onClick={() => setOpen(null)}>
                Close
              </button>
            </figcaption>
          </figure>
        </div>
      )}
    </>
  );
}

/** Is version `a` newer than `b`? Semver: a prerelease is older than its release. */
export function newer(a: string, b: string): boolean {
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

export const STAGES: Record<InstallProgress["stage"], string> = {
  finding: "Finding it…",
  downloading: "Downloading…",
  checking: "Checking it…",
  installing: "Installing…",
  done: "Installed",
  failed: "Didn't install",
};

/** The tier, as a chip. */
export function TierChip(props: { tier: StoreListing["tier"] | "unreviewed"; title?: string }) {
  switch (props.tier) {
    case "official":
      return (
        <span className="chip ok" title={props.title ?? "From the recorder's authors; reviewed"}>
          Official
        </span>
      );
    case "community":
      return (
        <span className="chip" title={props.title ?? "Third-party; reviewed"}>
          Community
        </span>
      );
    default:
      return (
        <span className="chip warn" title={props.title ?? "Not from the registry; unreviewed"}>
          Not reviewed
        </span>
      );
  }
}

/** "3 minutes ago", for when the list was fetched. */
function ago(t: number): string {
  const s = Math.max(0, Date.now() / 1000 - t);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  return new Date(t * 1000).toLocaleDateString();
}

export function StoreSource(props: { store: { source: "registry" | "saved" | "built-in"; fetched: number | null; problem: string | null } }) {
  const st = props.store;
  if (st.source === "registry") return <p className="muted small">Registry checked {st.fetched ? ago(st.fetched) : "now"}.</p>;
  const when = st.source === "saved" && st.fetched ? `the list from ${new Date(st.fetched * 1000).toLocaleString()}` : "the built-in list";
  return (
    <p className="small warn-text" title={st.problem ?? undefined}>
      Registry unreachable; showing {when}.
    </p>
  );
}


/** A store's listing: what it is, and `action` (Install / Update / Installed…). `chips`: more beside its tier. */
export function StoreCard(props: { l: StoreListing; icon: React.ReactNode; action: React.ReactNode; chips?: React.ReactNode }) {
  const { l } = props;
  const id = `store-${l.id}`;
  return (
    <article className="plugin-card store-card" aria-labelledby={id}>
      <div className="plugin-head">
        <span className="plugin-icon tone-off" aria-hidden="true">
          {props.icon}
        </span>
        <div className="plugin-title">
          <div className="row">
            <strong id={id}>{l.name}</strong>
            <span className="chip">v{l.version}</span>
            <TierChip tier={l.tier} />
            <Author repository={l.repository} />
            {props.chips}
          </div>
        </div>
        {props.action}
      </div>
      <p className="plugin-desc">{l.description}</p>
      {"screenshots" in l && <Screenshots name={l.name} shots={(l.screenshots ?? []).map((sh) => ({ src: `/store/screenshots/${sh.sha256}`, caption: sh.caption }))} />}
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

/** How an install under way is going, for a listing's button. */
export function InstallStage(props: { install: InstallProgress }) {
  return (
    <span className="small muted install-stage" role="status">
      {STAGES[props.install.stage]}
    </span>
  );
}

/** What differs between the stores' panels. */
export interface StoreWords {
  title: string;
  /** Search box examples. */
  searchHint: string;
  /** When the store has nothing. */
  empty: string;
  /** Above the GitHub form: what an unreviewed one may do. */
  warning: string;
  /** A repository, for the GitHub form's placeholder. */
  repoExample: string;
  /** Under it: which template's release workflow it must use. */
  template: string;
}

/** A store: its source, a search, its listings (each drawn by `card`), installs from GitHub under way, and the GitHub form. */
export function StorePanel<K extends StoreKind>(props: { kind: K; words: StoreWords; card: (l: ListingOf<K>) => React.ReactNode }) {
  const s = useApp();
  const { kind, words } = props;
  const store = s.stores[kind];
  const [query, setQuery] = useState("");
  useEffect(() => {
    if (!store) fetchStore(kind);
  }, [store, kind]);
  const q = query.trim().toLowerCase();
  const order = { official: 0, community: 1, unlisted: 2 };
  const all: StoreListing[] = store?.listings ?? [];
  const listings = all
    .filter((l) => !q || [l.id, l.name, l.description].some((f) => f.toLowerCase().includes(q)))
    .sort((a, b) => order[a.tier] - order[b.tier] || a.name.localeCompare(b.name));
  // Installs from GitHub, by repository: shown until they're done.
  const fromGitHub = Object.values(s.installs[kind]).filter((i) => !all.some((l) => l.id === i.key));
  const titleId = `${kind}-store-title`;
  return (
    <section className="panel" aria-labelledby={titleId}>
      <header className="panel-head">
        <h2 id={titleId}>{words.title}</h2>
        <button className="btn ghost small" onClick={() => fetchStore(kind, true)} disabled={!store}>
          Check again
        </button>
      </header>
      {!store ? (
        <p className="muted">Loading…</p>
      ) : (
        <div className="stack">
          <StoreSource store={store} />
          {all.length > 4 && (
            <div className="field">
              <label className="field-label" htmlFor={`${kind}-store-search`}>
                Search
              </label>
              <input id={`${kind}-store-search`} type="search" value={query} placeholder={words.searchHint} onChange={(e) => setQuery(e.target.value)} />
            </div>
          )}
          {listings.map((l) => props.card(l as ListingOf<K>))}
          {listings.length === 0 && <p className="empty">{q ? `Nothing matches “${query.trim()}”.` : words.empty}</p>}
          {fromGitHub.map((i) => (
            <p key={i.key} className="small muted" role="status">
              {i.key}: {STAGES[i.stage]}
            </p>
          ))}
          <div className="row">
            <InstallFromGitHub kind={kind} words={words} busy={fromGitHub.length > 0} />
          </div>
        </div>
      )}
    </section>
  );
}

/** One from a GitHub release that isn't in the store. */
function InstallFromGitHub(props: { kind: StoreKind; words: StoreWords; busy: boolean }) {
  const [open, setOpen] = useState(false);
  const [repo, setRepo] = useState("");
  const [tag, setTag] = useState("");
  if (!open)
    return (
      <button className="btn ghost" onClick={() => setOpen(true)}>
        Install from GitHub…
      </button>
    );
  const id = `${props.kind}-gh`;
  return (
    <form
      className="add-plugin"
      onSubmit={(e) => {
        e.preventDefault();
        if (!repo.trim()) return;
        installFromGitHub(props.kind, repo.trim(), tag);
        setRepo("");
        setTag("");
        setOpen(false);
      }}
    >
      <p className="small warn-text">{props.words.warning}</p>
      <div className="grid2">
        <div className="field">
          <label className="field-label" htmlFor={`${id}-repo`}>
            GitHub repository or release URL
          </label>
          <input id={`${id}-repo`} className="mono" autoFocus value={repo} placeholder={props.words.repoExample} onChange={(e) => setRepo(e.target.value)} />
        </div>
        <div className="field">
          <label className="field-label" htmlFor={`${id}-tag`}>
            Release (optional)
          </label>
          <input id={`${id}-tag`} className="mono" value={tag} placeholder="latest" onChange={(e) => setTag(e.target.value)} />
        </div>
      </div>
      <span className="field-hint">{props.words.template}</span>
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
