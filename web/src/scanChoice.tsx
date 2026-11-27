// What Find my system scans (SurveyScan): bands (the recorder's SurveyBands)
// and ranges of one's own. One picker for Setup → Find my system and the setup
// guide's scan step; the choice a scan last started with is remembered in this
// browser, both halves of it alike, and is what either page starts from.
import { useMemo, useState } from "react";
import { mhzShort, rangeList, rangeListProblem, type RangeList } from "./fieldText.ts";
import { DraftInput } from "./inputs.tsx";
import type { ScanRange, SurveyBand, SurveyScan } from "./protocol.ts";

/** The picker's state: the ranges with what was typed that isn't one, for the field to say so. */
export interface ScanPick {
  bands: string[];
  ranges: RangeList;
}

const KEY = "trp.surveyChoice";

const usual = (known: SurveyBand[]): ScanPick => ({ bands: known.filter((b) => b.defaultOn).map((b) => b.id), ranges: { ranges: [], bad: [] } });

/**
 * The choice the last scan here started with (of the bands still known; its ranges kept as typed and
 * read again, as the field reads them), else the bands on by default.
 */
export function savedPick(known: SurveyBand[]): ScanPick {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? "null") as { bands?: unknown; ranges?: unknown } | null;
    if (!v || !Array.isArray(v.bands) || typeof v.ranges !== "string") return usual(known);
    const bands = v.bands.filter((id): id is string => known.some((b) => b.id === id));
    const ranges = rangeList.parse(v.ranges);
    return bands.length || ranges.ranges.length ? { bands, ranges } : usual(known);
  } catch {
    return usual(known);
  }
}

/** Keeps `p` for next time: the bands, and the ranges as the field shows them. */
export function savePick(p: ScanPick): void {
  try {
    localStorage.setItem(KEY, JSON.stringify({ bands: p.bands, ranges: rangeList.format(p.ranges) }));
  } catch {
    // Storage blocked: chosen again next time.
  }
}

/** The same choice, read back from what's shown. */
export type ScanChoosing = ReturnType<typeof useScanChoice>;

/** Something to scan, and nothing typed that isn't a range. */
export function canScan(p: ScanPick): boolean {
  return (p.bands.length > 0 || p.ranges.ranges.length > 0) && !p.ranges.bad.length;
}

/**
 * The choice being made, `known` the recorder's bands: until one is picked,
 * the one last scanned with. `scan` is what to send; `ready` whether there is
 * anything to scan and nothing typed wrong, by the ranges as they are being
 * typed (`typing`: kept once the field is left, which pressing Scan does);
 * `remember` keeps it for next time.
 */
export function useScanChoice(known: SurveyBand[]) {
  const [picked, setPicked] = useState<ScanPick | null>(null);
  const [typing, setTyping] = useState<RangeList | undefined>(undefined);
  const saved = useMemo(() => savedPick(known), [known]);
  const pick = picked ?? saved;
  const scan: SurveyScan = { bands: pick.bands, ...(pick.ranges.ranges.length ? { ranges: pick.ranges.ranges } : {}) };
  return {
    pick,
    setPick: setPicked,
    typing,
    setTyping,
    scan,
    ready: canScan({ bands: pick.bands, ranges: typing ?? pick.ranges }),
    remember: () => savePick(pick),
  };
}

/** A band scanned, for the page: a band's name, or a range's "420–450 MHz". */
export function scanRangeLabel(r: ScanRange | null, known: SurveyBand[]): string {
  if (!r) return "";
  return (r.id !== null && known.find((b) => b.id === r.id)?.label) || `${mhzShort(r.loHz)}–${mhzShort(r.hiHz)} MHz`;
}

const HINT = "Anything the bands above don’t cover";

/** The bands to tick and the ranges to type: Setup's look (`setup`) or the setup guide's (`guide`). */
export function ScanPicker(props: { known: SurveyBand[]; choice: ScanChoosing; look: "setup" | "guide" }) {
  const { known, look } = props;
  const { pick, setPick: onChange, typing, setTyping } = props.choice;
  // While typing: what's wrong with it now (else with what was kept).
  const problem = rangeListProblem(typing ?? pick.ranges);
  const on = (id: string) => pick.bands.includes(id);
  const toggle = (id: string) => onChange({ ...pick, bands: on(id) ? pick.bands.filter((x) => x !== id) : [...pick.bands, id] });
  const span = (b: SurveyBand) => `${(b.loHz / 1e6).toFixed(0)}–${(b.hiHz / 1e6).toFixed(0)}`;
  const field = (className: string) => (
    <DraftInput
      className={className}
      aria-label="Other ranges, MHz"
      placeholder="420-450, 433-435"
      spellCheck={false}
      codec={rangeList}
      value={pick.ranges}
      onChange={(ranges) => onChange({ ...pick, ranges })}
      onDraft={setTyping}
    />
  );
  if (look === "guide") {
    return (
      <>
        <div className="ob-chips">
          {known.map((b) => (
            <button key={b.id} className={`ob-chip${on(b.id) ? " on" : ""}`} aria-pressed={on(b.id)} onClick={() => toggle(b.id)}>
              {b.label}
              <span className="ob-chip-sub">{span(b)} MHz</span>
            </button>
          ))}
        </div>
        <label className="ob-field">
          <span className="ob-label">Other ranges, MHz</span>
          {field("ob-input ob-mono")}
          <span className={`ob-hint${problem ? " bad" : ""}`}>{problem ?? HINT}</span>
        </label>
      </>
    );
  }
  return (
    <fieldset className="bands">
      <legend className="field-label">Bands to scan</legend>
      {known.map((b) => (
        <label key={b.id} className="toggle">
          <input type="checkbox" checked={on(b.id)} onChange={() => toggle(b.id)} />
          <span>
            {b.label} <span className="field-hint mono">{span(b)}</span>
          </span>
        </label>
      ))}
      <label className="field bands-own">
        <span className="field-label">Other ranges, MHz</span>
        {field("mono")}
        {problem ? <span className="warn small">{problem}</span> : <span className="field-hint">{HINT}</span>}
      </label>
    </fieldset>
  );
}
