// A system's talkgroups: rows of Trunk Recorder's talkgroup file columns
// (protocol.ts Talkgroup), as the recorder keeps them. A file is read and
// written by the recorder's own code (rules.ts, through wasm): CSV only at
// Load CSV and Download.
import type { Talkgroup, TalkgroupEdit } from "./protocol.ts";
import { talkgroupsEdit, talkgroupsFromCsv, talkgroupsToCsv, type TalkgroupFileReport } from "./rules.ts";

export type { Talkgroup };

/** Mode letters Trunk Recorder treats as "encrypted, don't record". */
export function isEncryptedMode(mode: string): boolean {
  return mode === "E" || mode === "TE" || mode === "DE";
}

/** Talkgroup `number`'s row with `t` set, the rest as a file row that leaves them empty (mode D, priority 1). */
export function talkgroup(number: number, t: Partial<Omit<Talkgroup, "number">> = {}): Talkgroup {
  return { number, mode: "D", alphaTag: "", description: "", tag: "", category: "", priority: 1, preferredNac: 0, preferredSite: "", ignore: false, ...t };
}

/** RFC-4180-ish line split: `delim` (a comma), double-quoted fields, "" escapes. */
export function splitCsvLine(line: string, delim = ","): string[] {
  const out: string[] = [];
  let cur = "";
  let q = false;
  for (let i = 0; i < line.length; i++) {
    const ch = line[i];
    if (q) {
      if (ch === '"' && line[i + 1] === '"') {
        cur += '"';
        i++;
      } else if (ch === '"') q = false;
      else cur += ch;
    } else if (ch === '"') q = true;
    else if (ch === delim) {
      out.push(cur.trim());
      cur = "";
    } else cur += ch;
  }
  out.push(cur.trim());
  return out;
}

/**
 * Mode letters RadioReference's talkgroup tables use (Trunk Recorder's too):
 * A analog, D digital, T TDMA, M mixed, E encrypted; DE / TE always
 * encrypted, De / Te only sometimes — Trunk Recorder records those, so the
 * case is kept.
 */
const RR_MODE = /^(A|D|T|E|M|DE|TE|De|Te|DM|AE|Ae)$/;

/**
 * A Mode cell: "D", or the letter with RadioReference's encryption badge
 * after it ("D ENC") — Trunk Recorder's encrypted modes (DE / TE / E), which
 * it doesn't record. Null when it isn't a mode.
 */
function rrMode(cell: string): string | null {
  const [raw = "", ...badges] = cell.split(/\s+/);
  // A lone lower-case letter ("d") is the upper-case one.
  const mode = raw.length === 1 ? raw.toUpperCase() : raw;
  if (!RR_MODE.test(mode)) return null;
  if (!badges.some((b) => /^enc/i.test(b)) || mode.endsWith("E")) return mode;
  return mode === "D" ? "DE" : mode === "T" ? "TE" : "E";
}

/**
 * A talkgroup table copied from RadioReference's web page (free to view, no
 * subscription): rows of DEC, HEX, Mode, Alpha Tag, Description, Tag — tab
 * separated when copied from a browser, else two or more spaces. A line with
 * no talkgroup number before a run of rows is the category heading they fall
 * under ("Metro County Fire"). The column header line is skipped.
 *
 * The page's badges come along: "ONLINE" after a number breaks the row in
 * two ("167" / "ONLINE<tab>0a7<tab>D…"), and "ENC" follows the mode ("D ENC").
 */
export function parseRadioReferencePaste(text: string): Talkgroup[] {
  const out: Talkgroup[] = [];
  const seen = new Set<number>();
  let category = "";
  // A number on a line of its own: the rest of its row is on the next line, after its badge.
  let pending: string | null = null;
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line) continue;
    let f = (raw.includes("\t") ? raw.split("\t") : line.split(/\s{2,}/)).map((x) => x.trim()).filter(Boolean);
    if (pending !== null) {
      const num = pending;
      pending = null;
      if (f.length > 1 && !/^\d+\b/.test(f[0])) f = [num, ...f.slice(1)];
      else out.push(row(Number(num), [], category));
    }
    if (/^dec\b/i.test(f[0] ?? "")) continue;
    // A badge in the number's cell ("167 ONLINE").
    const num = /^(\d+)(\s+[A-Za-z].*)?$/.exec(f[0] ?? "");
    if (!num) {
      // A heading: a category name, not a sentence of help text.
      if (f.length === 1 && line.length <= 60) category = line;
      continue;
    }
    if (f.length === 1) {
      pending = num[1];
      continue;
    }
    out.push(row(Number(num[1]), f.slice(1), category));
  }
  if (pending !== null) out.push(row(Number(pending), [], category));
  return out.filter((t) => !seen.has(t.number) && !!seen.add(t.number));
}

/** A talkgroup from the cells after its number: HEX (optional), Mode, Alpha Tag, Description, Tag. */
function row(number: number, f: string[], category: string): Talkgroup {
  let k = 0;
  // The HEX column, when copied (it is the same number).
  if (f[k] && /^[0-9a-f]+$/i.test(f[k]) && Number.parseInt(f[k], 16) === number) k++;
  const m = f[k] ? rrMode(f[k]) : null;
  if (m) k++;
  return talkgroup(number, { mode: m ?? "D", alphaTag: f[k] ?? "", description: f[k + 1] ?? "", tag: f[k + 2] ?? "", category });
}

/** A talkgroup file's text read as the recorder reads one: its rows, and what in it was passed over. */
export function readTalkgroupFile(text: string): TalkgroupFileReport {
  return talkgroupsFromCsv(text);
}

/** Talkgroups as a talkgroup file to download: Trunk Recorder's headed CSV. */
export function talkgroupFileText(rows: Talkgroup[]): string {
  return talkgroupsToCsv(rows);
}

/** The cells an edit of one talkgroup sets, by the file's column names (trunk-app's `TalkgroupEdit::cells`). */
export function editCells(e: TalkgroupEdit): [string, string][] {
  const text: [string, string | undefined][] = [
    ["Alpha Tag", e.alphaTag],
    ["Description", e.description],
    ["Tag", e.tag],
    ["Category", e.category],
  ];
  const cells = text.filter((c): c is [string, string] => c[1] !== undefined).map(([c, v]): [string, string] => [c, v.trim()]);
  if (e.ignore !== undefined) cells.push(["Ignore", e.ignore ? "x" : ""]);
  return cells;
}

/**
 * `rows` with talkgroup `tg`'s row edited: only the cells `edit` sets
 * change; a talkgroup without a row gets one when a value is set. Clearing
 * Ignore also undoes Trunk Recorder's way of ignoring, a priority of −1. The
 * recorder's own edit (trunk-core's `edit_rows`), so this copy holds what
 * the recorder holds.
 */
export function withEdit(rows: Talkgroup[], tg: number, edit: TalkgroupEdit): Talkgroup[] {
  return talkgroupsEdit(rows, tg, editCells(edit));
}

/** The edit that marks talkgroup `tg` ignored or not: named `alphaTag` too when `rows` lack it. */
export function ignoreEdit(rows: Talkgroup[], tg: number, ignore: boolean, alphaTag = ""): TalkgroupEdit {
  return ignore && alphaTag && !rows.some((t) => t.number === tg) ? { alphaTag, ignore } : { ignore };
}

/**
 * `existing` (a system's talkgroups) with those of `incoming` it doesn't
 * have added after them: its own rows stay as they are (a name of yours is
 * never replaced). `added`: how many were.
 */
export function mergeTalkgroups(existing: Talkgroup[], incoming: Talkgroup[]): { talkgroups: Talkgroup[]; added: number } {
  const have = new Set(existing.map((t) => t.number));
  const add = incoming.filter((t) => !have.has(t.number) && !!have.add(t.number));
  return { talkgroups: add.length ? [...existing, ...add] : existing, added: add.length };
}
