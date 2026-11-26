// What Find my system scans: bands (the recorder's SurveyBands) and ranges of
// one's own. One picker for Setup → Find my system and the setup guide's scan
// step; the choice a scan last started with is remembered in this browser,
// both halves of it alike, and is what either page starts from.
import { useMemo, useState } from "react";
import { mhzShort, rangeList, rangeListProblem, type RangeList } from "./fieldText.ts";
import { DraftInput } from "./inputs.tsx";
import type { HzRange, ScanRange, SurveyBand } from "./protocol.ts";

/** What a scan covers, as surveyStart takes it. */
export interface ScanChoice {
  bands: string[];
  ranges: HzRange[];
}

/** The picker's state: the ranges with what was typed that isn't one, for the field to say so. */
export interface ScanPick {
  bands: string[];
  ranges: RangeList;
}

const KEY = "trp.surveyChoice";

const usual = (known: SurveyBand[]): ScanPick => ({ bands: known.filter((b) => b.defaultOn).map((b) => b.id), ranges: { ranges: [], bad: [] } });

/** The choice the last scan here started with (of the bands still known), else the bands on by default. */
export function savedPick(known: SurveyBand[]): ScanPick {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? "null") as Partial<ScanChoice> | null;
    if (!v || !Array.isArray(v.bands) || !Array.isArray(v.ranges)) return usual(known);
    const bands = v.bands.filter((id) => known.some((b) => b.id === id));
    const ranges = v.ranges.filter((r) => typeof r?.loHz === "number" && typeof r?.hiHz === "number" && r.hiHz > r.loHz);
    return bands.length || ranges.length ? { bands, ranges: { ranges, bad: [] } } : usual(known);
  } catch {
    return usual(known);
  }
}

function save(c: ScanChoice): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(c));
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
  const scan: ScanChoice = { bands: pick.bands, ranges: pick.ranges.ranges };
  return {
    pick,
    setPick: setPicked,
    typing,
    setTyping,
    scan,
    ready: canScan({ bands: pick.bands, ranges: typing ?? pick.ranges }),
    remember: () => save(scan),
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
