// Plugin delays (the config's `pluginDelays`, and each system's): how long
// plugins wait for calls. Few people set any, so each place they're set is a
// disclosure: a system's card (its own rules), a plugin's card (its delay on
// every system) and the Plugins tab (every system's rules). Which rule wins
// is decided by the recorder (crates/trunk-pro/src/plugins/delay.rs);
// `inEffect` here follows the same rules, to show what they come to.

import { useEffect, useState } from "react";
import { systemAt, updateConfig, useApp, type SystemRef } from "./controller.ts";
import type { Config, DelayRule, PluginInfo } from "./protocol.ts";

/** Where a list of rules is: every system's (the top of the config), or one system's. */
type Scope = "every" | SystemRef;

function rulesAt(c: Config, at: Scope): DelayRule[] {
  return (at === "every" ? c.pluginDelays : systemAt(c, at)?.pluginDelays) ?? [];
}

function editRules(at: Scope, fn: (rules: DelayRule[]) => DelayRule[]): void {
  updateConfig((x) => {
    const owner = at === "every" ? x : systemAt(x, at);
    if (!owner) return;
    const next = fn([...(owner.pluginDelays ?? [])]);
    if (next.length) owner.pluginDelays = next;
    else delete owner.pluginDelays;
  });
}

/** "none", "45 s", "2 min", "1 h 30 min". */
export function delayText(s: number): string {
  if (!(s > 0)) return "none";
  if (s < 60) return `${Math.round(s)} s`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min`;
  return m % 60 ? `${Math.floor(m / 60)} h ${m % 60} min` : `${m / 60} h`;
}

// ── what's in effect: the recorder's rules ───────────────────────────────────

interface Rule {
  system: string | null;
  plugins: string[];
  talkgroups: string[];
  delayS: number;
}

const clean = (v: string[] | undefined) => [...new Set((v ?? []).map((x) => x.trim()).filter(Boolean))].sort();
const rule = (system: string | null, r: DelayRule): Rule => ({ system, plugins: clean(r.plugins), talkgroups: clean(r.talkgroups), delayS: r.delayS > 0 ? r.delayS : 0 });

function allRules(c: Config): Rule[] {
  return [
    ...(c.pluginDelays ?? []).map((r) => rule(null, r)),
    ...[...c.systems, ...c.conventional].flatMap((s) => (s.pluginDelays ?? []).map((r) => rule(s.shortName, r))),
  ];
}

/** A rule's plugin `p` (an id, or a copy's label) covers `label`. */
const covers = (p: string, label: string) => p === label || label.replace(/#\d+$/, "") === p;

/** `a` picks only what `b` picks. */
function within(a: Rule, b: Rule): boolean {
  return (
    (b.system === null || a.system === b.system) &&
    (!b.plugins.length || (a.plugins.length > 0 && a.plugins.every((p) => b.plugins.some((o) => covers(o, p))))) &&
    (!b.talkgroups.length || (a.talkgroups.length > 0 && a.talkgroups.every((t) => b.talkgroups.includes(t))))
  );
}

/**
 * How long plugin `id` waits on system `system` for the talkgroups `tgs` (a
 * rule's list, as written), or (null) for those no rule names: a narrower
 * rule replaces what it narrows, then the longest wins.
 */
export function inEffect(c: Config, id: string, system: string, tgs: string[] | null): number {
  const picked = allRules(c).filter(
    (r) =>
      (r.system === null || r.system === system) &&
      (!r.plugins.length || r.plugins.some((p) => covers(p, id))) &&
      (!r.talkgroups.length || (tgs !== null && tgs.every((t) => r.talkgroups.includes(t)))),
  );
  const narrows = (a: Rule, b: Rule) => within(a, b) && !within(b, a);
  return Math.max(0, ...picked.filter((r) => !picked.some((o) => narrows(o, r))).map((r) => r.delayS));
}

/** The plugins on that take calls (what delays are for). */
function takers(c: Config, list: PluginInfo[]): PluginInfo[] {
  return list.filter((p) => c.plugins?.[p.id]?.enabled && p.manifest && (p.manifest.subscribe.includes("call.concluded") || p.manifest.subscribe.includes("audio")));
}

const nameOf = (list: PluginInfo[], id: string) => list.find((p) => p.id === id.replace(/#\d+$/, ""))?.manifest?.name ?? id;

// ── inputs ───────────────────────────────────────────────────────────────────

/** A delay in minutes (decimals allowed); empty: none. */
function MinutesInput(props: { s: number; label: string; onChange: (s: number) => void }) {
  const shown = (s: number) => (s > 0 ? String(Math.round((s / 60) * 100) / 100) : "");
  const [text, setText] = useState(shown(props.s));
  useEffect(() => setText((t) => (parse(t) === props.s ? t : shown(props.s))), [props.s]); // eslint-disable-line react-hooks/exhaustive-deps
  function parse(t: string): number {
    const v = Number(t.trim().replace(",", "."));
    return t.trim() === "" || !Number.isFinite(v) || v < 0 ? 0 : Math.round(v * 60);
  }
  return (
    <input
      className="mono narrow"
      inputMode="decimal"
      aria-label={props.label}
      placeholder="none"
      value={text}
      onChange={(e) => {
        setText(e.target.value);
        props.onChange(parse(e.target.value));
      }}
    />
  );
}

/** Talkgroups as typed ("1001, 1000-1099, 507*"); empty: all. */
function TalkgroupsInput(props: { value: string[]; onChange: (v: string[]) => void }) {
  const split = (t: string) => t.split(/[\s,;]+/).filter(Boolean);
  const [text, setText] = useState(props.value.join(", "));
  useEffect(() => setText((t) => (split(t).join(",") === props.value.join(",") ? t : props.value.join(", "))), [props.value]); // eslint-disable-line react-hooks/exhaustive-deps
  return (
    <input
      className="mono"
      aria-label="Talkgroups"
      placeholder="all"
      value={text}
      onChange={(e) => {
        setText(e.target.value);
        props.onChange(split(e.target.value));
      }}
    />
  );
}

/** A list of rules: talkgroups, plugin and delay each. */
function RuleTable(props: { at: Scope; rules: DelayRule[]; plugins: PluginInfo[] }) {
  const { at, rules, plugins } = props;
  const edit = (i: number, fn: (r: DelayRule) => void) =>
    editRules(at, (rs) => {
      const r = { ...rs[i] };
      fn(r);
      if (!r.plugins?.length) delete r.plugins;
      if (!r.talkgroups?.length) delete r.talkgroups;
      rs[i] = r;
      return rs;
    });
  // Plugins to choose from: those installed, and any a rule names that isn't (a copy, one removed by hand).
  const ids = [...new Set([...plugins.map((p) => p.id), ...rules.flatMap((r) => (r.plugins?.length === 1 ? r.plugins : []))])];
  return (
    <>
      {rules.length > 0 && (
        <div className="table-wrap channels-wrap">
          <table className="calls channels delays">
            <thead>
              <tr>
                <th title="Numbers, ranges (1000-1099) and patterns (507*); empty: all">Talkgroups</th>
                <th>Plugin</th>
                <th>Delay, min</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {rules.map((r, i) => {
                const several = (r.plugins?.length ?? 0) > 1 ? r.plugins!.join(",") : null;
                return (
                  <tr key={i}>
                    <td>
                      <TalkgroupsInput value={r.talkgroups ?? []} onChange={(v) => edit(i, (x) => void (x.talkgroups = v))} />
                    </td>
                    <td>
                      <select
                        aria-label="Plugin"
                        value={several ?? r.plugins?.[0] ?? ""}
                        onChange={(e) => edit(i, (x) => void (x.plugins = e.target.value === several ? r.plugins : e.target.value ? [e.target.value] : []))}
                      >
                        <option value="">Every plugin</option>
                        {several && <option value={several}>{r.plugins!.map((p) => nameOf(plugins, p)).join(", ")}</option>}
                        {ids.map((id) => (
                          <option key={id} value={id}>
                            {nameOf(plugins, id)}
                            {id.includes("#") ? ` (copy ${id.split("#")[1]})` : ""}
                          </option>
                        ))}
                      </select>
                    </td>
                    <td>
                      <MinutesInput s={r.delayS} label="Delay, min" onChange={(s) => edit(i, (x) => void (x.delayS = s))} />
                    </td>
                    <td>
                      <button type="button" className="btn ghost small danger" aria-label={`Remove delay ${i + 1}`} onClick={() => editRules(at, (rs) => rs.filter((_, j) => j !== i))}>
                        Remove
                      </button>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
      <div className="row">
        <button type="button" className="btn ghost small" onClick={() => editRules(at, (rs) => [...rs, { delayS: 600 }])}>
          + Add a delay
        </button>
      </div>
    </>
  );
}

/** What the rules come to on one system, for each plugin that takes calls. */
function InEffect(props: { c: Config; system: string; plugins: PluginInfo[] }) {
  const { c, system, plugins } = props;
  // Each list of talkgroups a rule for this system (or every system) names.
  const lists = [
    ...new Map(
      allRules(c)
        .filter((r) => (r.system === null || r.system === system) && r.talkgroups.length)
        .map((r) => [r.talkgroups.join(","), r.talkgroups]),
    ).values(),
  ];
  const rows = plugins.map((p) => {
    const rest = inEffect(c, p.id, system, null);
    const own = lists.map((tgs) => ({ tgs, s: inEffect(c, p.id, system, tgs) })).filter((x) => x.s !== rest);
    return { p, rest, own };
  });
  if (rows.every((r) => !r.rest && !r.own.length)) return null;
  return (
    <div className="delay-effect">
      <span className="field-label">In effect on {system}</span>
      <ul>
        {rows.map(({ p, rest, own }) => (
          <li key={p.id}>
            <strong>{p.manifest!.name}</strong>: {own.length ? `${delayText(rest)} for other talkgroups` : delayText(rest)}
            {own.map((x) => (
              <span key={x.tgs.join(",")}>
                ; <span className="mono">{x.tgs.join(", ")}</span>: {delayText(x.s)}
              </span>
            ))}
          </li>
        ))}
      </ul>
    </div>
  );
}

const WHAT =
  "Hold calls back from plugins for a while, for talkgroups that are sensitive or a service that asks for a delay. A delayed call's live audio isn't streamed.";
const WINS = "A row for some talkgroups, or for one plugin, replaces a broader one; otherwise the longest delay applies.";

/** A system's own delays (its card's Overrides). */
export function SystemDelays(props: { system: SystemRef }) {
  const s = useApp();
  const c = s.config;
  const sys = c && systemAt(c, props.system);
  if (!c || !sys?.shortName) return null;
  const rules = rulesAt(c, props.system);
  const plugins = takers(c, s.plugins?.plugins ?? []);
  const fromEvery = plugins.some((p) => inEffect(c, p.id, sys.shortName, null) > 0);
  return (
    <details className="help override">
      <summary>
        Plugin delays
        <span className="muted">{rules.length ? ` — ${rules.length} here` : fromEvery ? " — from the Plugins tab" : " — none"}</span>
      </summary>
      <p className="field-hint">
        {WHAT} {WINS} These come before the Plugins tab's.
      </p>
      <RuleTable at={props.system} rules={rules} plugins={plugins} />
      <InEffect c={c} system={sys.shortName} plugins={plugins} />
    </details>
  );
}

/** A plugin's delay on every system (its card on Setup's Plugins tab): the rule for it alone. */
export function PluginDelay(props: { p: PluginInfo; c: Config }) {
  const { p, c } = props;
  const rules = c.pluginDelays ?? [];
  const mine = rules.findIndex((r) => r.plugins?.length === 1 && r.plugins[0] === p.id && !r.talkgroups?.length);
  const s = mine < 0 ? 0 : rules[mine].delayS;
  const set = (v: number) =>
    editRules("every", (rs) => {
      if (mine >= 0 && !v) return rs.filter((_, j) => j !== mine);
      if (mine >= 0) rs[mine] = { ...rs[mine], delayS: v };
      else if (v) rs.push({ plugins: [p.id], delayS: v });
      return rs;
    });
  // Systems where it comes to something else.
  const other = [...c.systems, ...c.conventional]
    .filter((x) => x.shortName && x.enabled)
    .map((x) => ({ name: x.shortName, s: inEffect(c, p.id, x.shortName, null) }))
    .filter((x) => x.s !== s);
  return (
    <details className="help override plugin-delay">
      <summary>
        Delay
        <span className="muted"> — {delayText(s)}</span>
      </summary>
      <label className="field">
        <span className="field-label">Delay on every system, min</span>
        <MinutesInput s={s} label="Delay, min" onChange={set} />
        <span className="field-hint">Calls reach it this long after they end. A system can change it, and delay talkgroups, on its card</span>
      </label>
      {other.length > 0 && (
        <p className="field-hint">
          With systems' own delays: {other.map((x) => `${x.name} ${delayText(x.s)}`).join(", ")} (other talkgroups).
        </p>
      )}
    </details>
  );
}

/** Delays for every system (Setup's Plugins tab), each plugin's own included. */
export function EveryDelays() {
  const s = useApp();
  const c = s.config;
  if (!c) return null;
  const rules = rulesAt(c, "every");
  const plugins = takers(c, s.plugins?.plugins ?? []);
  return (
    <details className="help override">
      <summary>
        Plugin delays on every system
        <span className="muted">{rules.length ? ` — ${rules.length}` : " — none"}</span>
      </summary>
      <p className="field-hint">
        {WHAT} {WINS} A system's own, on its card, come first.
      </p>
      <RuleTable at="every" rules={rules} plugins={plugins} />
    </details>
  );
}
