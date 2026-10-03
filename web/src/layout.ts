// How the waterfalls share rows: narrow sources two to a row, wide ones a row
// each, and a narrow one with no narrow partner a row to itself.

/** Sources this wide (Hz) get a whole row for their waterfall. */
export const WIDE_SOURCE_HZ = 5e6;

/**
 * Which waterfalls take a whole row, given each source's rate in order (null:
 * no spectrum yet, not shown). A narrow source pairs with the next shown one
 * when that is narrow too; otherwise it has the row to itself.
 */
export function fullRows(rates: (number | null)[]): boolean[] {
  const shown = rates.map((r, i) => (r === null ? -1 : i)).filter((i) => i >= 0);
  const full = rates.map(() => false);
  let k = 0;
  while (k < shown.length) {
    const i = shown[k];
    const next = shown[k + 1];
    const wide = (j: number | undefined) => j === undefined || (rates[j] ?? 0) >= WIDE_SOURCE_HZ;
    if (wide(i) || wide(next)) {
      full[i] = true;
      k += 1;
    } else {
      // A pair: this one and the next share the row.
      k += 2;
    }
  }
  return full;
}
