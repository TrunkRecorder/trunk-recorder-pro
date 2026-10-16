// What the plugin store and the interface store share on screen: version
// order, how an install is going, a listing's tier, and how fresh the list is.

import { useState } from "react";
import type { PluginInstall, StoreListing } from "./protocol.ts";

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

export const STAGES: Record<PluginInstall["stage"], string> = {
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

