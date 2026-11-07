// Advice for the sources: better centres and rates than they have
// (sourceAdvice, the recorder's Config::advice), shown where they're set —
// a line on each source's card, and what it all does above them — and
// taken with one edit. What's heard on the air weighs it: each system's
// frequencies by their calls in the last week (the radio registry's).

import { useMemo } from "react";
import { activeSystems, foundRadio, formatMhz, sourceAdvice, type Advice, type Heard } from "./config.ts";
import { radioQuery, setSetupTab, setView, updateConfig, usePolled, useApp } from "./controller.ts";
import { msps as rate } from "./fieldText.ts";
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

const msps = (hz: number) => `${rate.format(hz)} MSPS`;

/** What taking the advice does, a phrase each ("the sources would …"): needs met, more reached, radios freed, sample rate saved. */
export function adviceReasons(a: Advice): string[] {
  const { before: b, after: x } = a;
  const out: string[] = [];
  if (x.unmet < b.unmet) out.push(`reach what ${b.unmet - x.unmet === 1 ? "a system needs" : `${b.unmet - x.unmet} systems need`} that no source reaches now`);
  if (x.reached > b.reached + 1e-9) out.push("reach more of the channels the systems use");
  if (x.needed < b.needed) out.push(`need ${b.needed - x.needed === 1 ? "one radio" : `${b.needed - x.needed} radios`} fewer`);
  if (x.unsteady < b.unsteady) out.push("run no RTL-SDR past its dependable 2.56 MSPS");
  if (x.rateHz < b.rateHz - 1) out.push(`sample ${msps(b.rateHz - x.rateHz)} less in all (less CPU and USB)`);
  return out;
}

/** "a", "a and b", "a, b and c". */
const listed = (xs: string[]) => (xs.length > 1 ? `${xs.slice(0, -1).join(", ")} and ${xs[xs.length - 1]}` : (xs[0] ?? ""));

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
    return <span className="field-hint">With the suggested settings, the other sources reach everything this one does: it can be removed.</span>;
  }
  if (!s.changed) return null;
  const parts: string[] = [];
  const moves = src.centerHz ? s.centerHz !== null && Math.abs(s.centerHz - src.centerHz) >= 1000 : false;
  if (moves) parts.push(s.auto ? `auto (${formatMhz(s.centerHz!, 4)} MHz)` : `${formatMhz(s.centerHz!, 4)} MHz`);
  if (Math.abs(s.rateHz - src.rateHz) >= 1) parts.push(msps(s.rateHz));
  if (!parts.length) return null;
  return (
    <span className="field-hint">
      Suggested: <span className="mono">{parts.join(" at ")}</span>
      {props.advice.sources.filter((x) => x.changed).length > 1 && " (with the other suggestions)"} ·{" "}
      <button className="btn ghost small" onClick={() => takeAdvice(props.advice!, props.i)}>
        Use
      </button>
    </span>
  );
}

/** On the RF page: that Setup has suggested settings for the sources, what they'd do, and the way there. */
export function AdviceNote(props: { advice: Advice | null }) {
  const reasons = props.advice ? adviceReasons(props.advice) : [];
  if (!reasons.length) return null;
  const open = () => {
    setView("setup");
    setSetupTab("radios");
  };
  return (
    <div className="banner">
      <span>With the settings Setup suggests, the sources would {listed(reasons)}.</span>
      <button className="btn small" onClick={open}>
        Review in Setup
      </button>
    </div>
  );
}

/** Above the sources: what the suggested settings do, and Apply all. */
export function SourcesAdvice(props: { advice: Advice | null }) {
  const a = props.advice;
  const reasons = a ? adviceReasons(a) : [];
  if (!a || !reasons.length) return null;
  return (
    <div className="banner">
      <span>
        With the suggested settings, the sources would {listed(reasons)}. Each source's card says what changes.
      </span>
      <button className="btn small" onClick={() => takeAdvice(a)}>
        Apply all
      </button>
    </div>
  );
}
