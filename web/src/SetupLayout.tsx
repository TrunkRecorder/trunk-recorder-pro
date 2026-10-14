// The setup page's layout, in three levels: a section (a heading over its
// cards), a card (one system, source or panel) and, inside a card, groups of
// related settings, each named on the left with what it is for.
// The rules for using them: docs/developer/ui-style.md.

import { useId, useSyncExternalStore, type CSSProperties, type ReactNode } from "react";

/** A part of a setup tab: a heading, what it is for, its actions, then its cards. */
export function Section(props: { title: string; hint?: ReactNode; actions?: ReactNode; children: ReactNode; id?: string }) {
  return (
    <section className="set-section" id={props.id}>
      <header className="set-section-head">
        <div className="set-section-title">
          <h2>{props.title}</h2>
          {props.hint && <p>{props.hint}</p>}
        </div>
        {props.actions && <div className="row">{props.actions}</div>}
      </header>
      {props.children}
    </section>
  );
}

/**
 * Related settings in a card. `cols`: fields per row (toggles, hints and
 * wide fields take the whole row); `full`: the name above, the settings the
 * card's whole width (a table); `actions`: buttons beside the name.
 */
export function Group(props: { title: string; hint?: ReactNode; actions?: ReactNode; children: ReactNode; cols?: 1 | 2 | 3; full?: boolean }) {
  return (
    <div className={`set-group${props.full ? " full" : ""}`}>
      <div className="set-group-head">
        <div>
          <h4>{props.title}</h4>
          {props.hint && <p>{props.hint}</p>}
        </div>
        {props.actions && <div className="row">{props.actions}</div>}
      </div>
      <div className={`set-group-body cols-${props.cols ?? 2}`}>{props.children}</div>
    </div>
  );
}

/** A card's on/off switch ("Record"). */
export function Switch(props: { label: string; checked: boolean; onChange: (v: boolean) => void; title?: string }) {
  return (
    <label className="switch" title={props.title}>
      <input type="checkbox" role="switch" checked={props.checked} onChange={(e) => props.onChange(e.target.checked)} />
      <span className="switch-track" aria-hidden="true" />
      <span>{props.label}</span>
    </label>
  );
}

/*
 * Which cards are open, by key ("sys-0", "conv-1", "src-2"), remembered in
 * this browser. A key not set falls back to the card's default.
 */
const OPEN_KEY = "trp.setupOpen";
let opened: Record<string, boolean> = (() => {
  try {
    return JSON.parse(localStorage.getItem(OPEN_KEY) ?? "{}") as Record<string, boolean>;
  } catch {
    return {};
  }
})();
const listeners = new Set<() => void>();

function saveOpened(next: Record<string, boolean>): void {
  opened = next;
  try {
    localStorage.setItem(OPEN_KEY, JSON.stringify(opened));
  } catch {
    // Not kept; still applies until reload.
  }
  listeners.forEach((l) => l());
}

function useOpened(): Record<string, boolean> {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => void listeners.delete(l);
    },
    () => opened,
  );
}

/** Open or close one card. */
export function setCardOpen(key: string, open: boolean): void {
  saveOpened({ ...opened, [key]: open });
}

/** Open or close a list's cards (`${prefix}0` … `${prefix}${n - 1}`). */
export function setCardsOpen(prefix: string, n: number, open: boolean): void {
  const next = { ...opened };
  for (let k = 0; k < n; k++) next[`${prefix}${k}`] = open;
  saveOpened(next);
}

/** A card is added to a list of n: it opens, and the others stay as they look now. */
export function addCard(prefix: string, n: number): void {
  const next = { ...opened };
  for (let k = 0; k < n; k++) next[`${prefix}${k}`] ??= openByDefault(n);
  next[`${prefix}${n}`] = true;
  saveOpened(next);
}

/** Card k of a list of n is gone: the ones after it move up, and keep their state. */
export function forgetCard(prefix: string, k: number, n: number): void {
  const next = { ...opened };
  for (let j = k; j < n - 1; j++) {
    const v = opened[`${prefix}${j + 1}`];
    if (v === undefined) delete next[`${prefix}${j}`];
    else next[`${prefix}${j}`] = v;
  }
  delete next[`${prefix}${n - 1}`];
  saveOpened(next);
}

/** Whether a list's cards start open: only when it has just one. */
export const openByDefault = (n: number) => n <= 1;

/** Whether every card of a list is open. */
export function useAllOpen(prefix: string, n: number): boolean {
  const o = useOpened();
  return Array.from({ length: n }, (_, k) => o[`${prefix}${k}`] ?? openByDefault(n)).every(Boolean);
}

/** "Expand all" / "Collapse all", for a list of more than one card. */
export function ExpandAll(props: { prefix: string; n: number }) {
  const all = useAllOpen(props.prefix, props.n);
  if (props.n < 2) return null;
  return (
    <button className="btn ghost" onClick={() => setCardsOpen(props.prefix, props.n, !all)}>
      {all ? "Collapse all" : "Expand all"}
    </button>
  );
}

/**
 * A setup card that folds to its head. `head`: what it is (its name, a line
 * about it, chips), the toggle; `actions`: its switch and buttons, beside it.
 */
export function Card(props: {
  cardKey: string;
  n: number;
  head: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  className?: string;
  style?: CSSProperties;
  id?: string;
  /** Above the groups, open or not (what an import left to do here). */
  note?: ReactNode;
}) {
  const o = useOpened();
  const open = o[props.cardKey] ?? openByDefault(props.n);
  const body = useId();
  return (
    <section
      className={`set-card${open ? "" : " collapsed"}${props.className ? ` ${props.className}` : ""}`}
      style={props.style}
      id={props.id}
      data-card={props.cardKey}
      data-collapsed={open ? undefined : "true"}
    >
      <header className="set-card-head foldable">
        <button type="button" className="set-card-toggle" aria-expanded={open} aria-controls={body} onClick={() => setCardOpen(props.cardKey, !open)}>
          <svg className="set-card-chevron" viewBox="0 0 16 16" aria-hidden="true">
            <path d="M6 3.5 10.5 8 6 12.5" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
          {props.head}
        </button>
        {props.actions && <div className="row set-card-actions">{props.actions}</div>}
      </header>
      {props.note}
      <div className="set-card-body" id={body} hidden={!open}>
        {props.children}
      </div>
    </section>
  );
}
