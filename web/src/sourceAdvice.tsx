// Advice for the sources: better centres and rates than they have
// (sourceAdvice, the recorder's Config::advice), shown where they're set —
// a line on each source's card, and what it all does above them — and
// taken with one edit. What's heard on the air weighs it: each system's
// frequencies by their calls in the last week (the radio registry's).

import { useMemo } from "react";
import { activeSystems, foundRadio, formatMhz, sourceAdvice, type Advice, type Heard } from "./config.ts";
import { radioQuery, setSetupTab, setView, updateConfig, usePolled, useApp } from "./controller.ts";
import { msps as rate } from "./fieldText.ts";
import { Fig, Figs, Info } from "./charts.tsx";
import type { Config, RadioResult } from "./protocol.ts";

/** How far back what's heard is counted, hours. */
const HEARD_HOURS = 168;

/** What each system was heard on: each frequency's calls, decoded and out of band, from the registry's `freqs` answers. */
export function heardOf(answers: [string, RadioResult | null][]): Heard {
  const out: Heard = {};
  for (const [name, r] of answers) {
    const calls = new Map<number, number>();
    for (const row of [...(r?.rows ?? []), ...(r?.outOfBand ?? [])] as { freqHz: number; calls: number }[]) {
      if (row.calls > 0) calls.set(row.freqHz, (calls.get(row.freqHz) ?? 0) + row.calls);
    }
    if (calls.size) out[name] = [...calls].map(([hz, weight]) => ({ hz, weight }));
  }
  return out;
}

/** Advice for `c`'s sources (null: none): their radios as found, and what each trunked system was heard on. */
export function useSourceAdvice(c: Config | null): Advice | null {
  const s = useApp();
  const names = c ? activeSystems(c).map((x) => x.shortName) : [];
  const answers = usePolled(`heard:${names.join(",")}`, () => Promise.all(names.map((n) => radioQuery({ what: "freqs", system: n, hours: HEARD_HOURS }).then((r): [string, RadioResult | null] => [n, r], (): [string, RadioResult | null] => [n, null]))), 300_000);
  return useMemo(() => (c ? sourceAdvice(c, c.sources.map((x) => foundRadio(s.radios, x)), heardOf(answers ?? [])) : null), [c, s.radios, answers]);
}

/** What taking the advice does, a figure each with a short label: needs met, more reached, radios freed, sample rate saved. */
export function adviceFigs(a: Advice): { k: string; v: string; u?: string; title?: string }[] {
  const { before: b, after: x } = a;
  const out: { k: string; v: string; u?: string; title?: string }[] = [];
  if (x.unmet < b.unmet) out.push({ k: "Systems missed", v: `${b.unmet} → ${x.unmet}`, title: "Systems or sites whose channels no source reaches" });
  if (x.reached > b.reached + 1e-9) out.push({ k: "Channels reached", v: b.reached > 0 ? `+${Math.max(1, Math.round((x.reached / b.reached - 1) * 100))}` : "more", u: b.reached > 0 ? "%" : undefined, title: "The channels the systems use, weighed by calls heard" });
  if (x.needed < b.needed) out.push({ k: "Radios needed", v: `${b.needed} → ${x.needed}` });
  if (x.rateHz < b.rateHz - 1) out.push({ k: "Sample rate", v: `−${rate.format(b.rateHz - x.rateHz)}`, u: "MSPS", title: "Less CPU and USB" });
  return out;
}

/** Take the advice for source `i` alone (back to auto when auto puts it there, else its centre written in), or (undefined) all of it. */
export function takeAdvice(a: Advice, i?: number): void {
  updateConfig((c) => {
    a.sources.forEach((s, k) => {
      if ((i !== undefined && k !== i) || !s.changed) return;
      const x = c.sources[k];
      x.rateHz = s.rateHz;
      const onAuto = i === undefined ? s.released : s.auto;
      if (onAuto) x.centerHz = 0;
      else if (x.centerHz && s.centerHz !== null) x.centerHz = s.centerHz;
    });
    // (All of it: every source on auto in the advice is, so auto places them as it says.)
    if (i === undefined) a.sources.forEach((s, k) => s.released && (c.sources[k].centerHz = 0));
  });
}

/** A source's line of advice, on its card: what to set, and Use; or that nothing would need it. */
export function SourceSuggestion(props: { c: Config; i: number; advice: Advice | null }) {
  const s = props.advice?.sources[props.i];
  const src = props.c.sources[props.i];
  if (!s || !props.advice) return null;
  if (!s.needed && props.advice.before.needed > props.advice.after.needed) {
    return (
      <span className="field-hint advice-line">
        Suggested <Fig v="Remove" level="warn" />
        <Info>With the suggested settings, the other sources reach everything this one does.</Info>
      </span>
    );
  }
  if (!s.changed) return null;
  const moves = src.centerHz ? s.centerHz !== null && Math.abs(s.centerHz - src.centerHz) >= 1000 : false;
  const rateMoves = Math.abs(s.rateHz - src.rateHz) >= 1;
  if (!moves && !rateMoves) return null;
  const others = props.advice.sources.filter((x) => x.changed).length > 1;
  return (
    <span className="field-hint advice-line" title={others ? "Together with the other sources' suggestions" : undefined}>
      Suggested
      {moves && (
        <>
          {s.auto && <span className="chip">auto</span>}
          <Fig v={formatMhz(s.centerHz!, 4)} u="MHz" />
        </>
      )}
      {rateMoves && <Fig v={rate.format(s.rateHz)} u="MSPS" />}
      <button className="btn ghost small" onClick={() => takeAdvice(props.advice!, props.i)}>
        Use
      </button>
    </span>
  );
}

/** On the RF page: that Setup has suggested settings for the sources, what they'd do, and the way there. */
export function AdviceNote(props: { advice: Advice | null }) {
  const figs = props.advice ? adviceFigs(props.advice) : [];
  if (!figs.length) return null;
  const open = () => {
    setView("setup");
    setSetupTab("radios");
  };
  return (
    <div className="banner advice-banner">
      <b>Suggested settings</b>
      <Figs items={figs} />
      <button className="btn small" onClick={open}>
        Review in Setup
      </button>
    </div>
  );
}

/** Above the sources: what the suggested settings do, and Apply all (each source's card shows its own change). */
export function SourcesAdvice(props: { advice: Advice | null }) {
  const a = props.advice;
  const figs = a ? adviceFigs(a) : [];
  if (!a || !figs.length) return null;
  return (
    <div className="banner advice-banner">
      <b>Suggested settings</b>
      <Figs items={figs} />
      <button className="btn small" onClick={() => takeAdvice(a)} title="Each source's card shows its change">
        Apply all
      </button>
    </div>
  );
}
