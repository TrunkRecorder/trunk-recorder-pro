// Trunk Recorder talkgroup CSV (trunk-recorder/talkgroups.cc): either the
// headed format (first column "Decimal"; any of Decimal, Hex, Mode, Alpha Tag,
// Description, Tag, Category, Priority, Preferred NAC, Comment in any order) or
// the legacy headerless one: Decimal,Hex,Mode,Alpha Tag,Description,Tag,Group[,Priority].
import type { TalkgroupEdit } from "./protocol.ts";

export interface Talkgroup {
  number: number;
  mode: string;
  alphaTag: string;
  description: string;
  tag: string;
  group: string;
  priority: number;
  preferredNac: number;
  /** Never record it: the Ignore column, or Trunk Recorder's priority −1. */
  ignore?: boolean;
}

/** Mode letters Trunk Recorder treats as "encrypted, don't record". */
export function isEncryptedMode(mode: string): boolean {
  return mode === "E" || mode === "TE" || mode === "DE";
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

const int = (s: string | undefined, dflt = 0): number => {
  const v = Number.parseInt(s ?? "", 10);
  return Number.isFinite(v) ? v : dflt;
};

/**
 * The delimiter a CSV's first line uses: comma, semicolon, tab or bar (what
 * Trunk Recorder's CSV reader guesses between; spreadsheets set to a
 * decimal-comma locale save with semicolons).
 */
export function csvDelimiter(line: string): string {
  let best = ",";
  let most = 0;
  for (const d of [",", ";", "\t", "|"]) {
    const n = line.split(d).length - 1;
    if (n > most) [best, most] = [d, n];
  }
  return best;
}

/** Header names as Trunk Recorder spells them, by any case and the names RadioReference's export uses. */
const HEADER: Record<string, string> = {
  decimal: "Decimal",
  dec: "Decimal",
  hex: "Hex",
  mode: "Mode",
  "alpha tag": "Alpha Tag",
  alphatag: "Alpha Tag",
  description: "Description",
  tag: "Tag",
  category: "Category",
  group: "Category",
  priority: "Priority",
  "preferred nac": "Preferred NAC",
  "preferred site": "Preferred Site",
  ignore: "Ignore",
  comment: "Comment",
};
const canonical = (h: string) => HEADER[h.trim().toLowerCase()] ?? h.trim();

/** Lines that hold something: not blank, not a # comment, without a spreadsheet's byte-order mark. */
function dataLines(text: string): string[] {
  return text
    .replace(/^\uFEFF/, "")
    .split(/\r?\n/)
    .filter((l) => l.trim() && !l.trim().startsWith("#"));
}

export function parseTalkgroupCsv(text: string): Map<number, Talkgroup> {
  return readTalkgroupCsv(text).talkgroups;
}

/** What a talkgroup file held, and what in it was passed over. */
export interface TalkgroupCsvReport {
  talkgroups: Map<number, Talkgroup>;
  /** No header: the old fixed columns (Decimal, Hex, Mode, Alpha Tag, Description, Tag, Group, Priority). */
  legacy: boolean;
  /** Rows with no talkgroup number in the Decimal column (their text, the first few). */
  skipped: string[];
  /** Columns neither Trunk Recorder nor this app knows. */
  unknownColumns: string[];
  /** Header problems Trunk Recorder would stop at (it needs Decimal, Mode and Description). */
  missing: string[];
}

export function readTalkgroupCsv(text: string): TalkgroupCsvReport {
  const lines = dataLines(text);
  const out: TalkgroupCsvReport = { talkgroups: new Map(), legacy: false, skipped: [], unknownColumns: [], missing: [] };
  if (!lines.length) return out;
  const delim = csvDelimiter(lines[0]);
  const first = splitCsvLine(lines[0], delim).map(canonical);
  const headed = first[0] === "Decimal";
  out.legacy = !headed;
  if (headed) {
    out.unknownColumns = first.filter((h) => h && !Object.values(HEADER).includes(h));
    out.missing = ["Mode", "Description"].filter((h) => !first.includes(h));
  }
  const col = (name: string) => first.indexOf(name);
  const rows = headed ? lines.slice(1) : lines;
  for (const line of rows) {
    const f = splitCsvLine(line, delim);
    const get = (name: string, legacy: number) => (headed ? (col(name) >= 0 ? f[col(name)] : undefined) : f[legacy]);
    const cell = (get("Decimal", 0) ?? "").trim();
    const number = /^\d+$/.test(cell) ? Number(cell) : NaN;
    if (!Number.isFinite(number)) {
      out.skipped.push(line.trim());
      continue;
    }
    out.talkgroups.set(number, {
      number,
      mode: get("Mode", 2) ?? "",
      alphaTag: get("Alpha Tag", 3) ?? "",
      description: get("Description", 4) ?? "",
      tag: get("Tag", 5) ?? "",
      group: get("Category", 6) ?? "",
      priority: int(get("Priority", 7), 1),
      preferredNac: int(get("Preferred NAC", 99), 0),
      ignore: /^(true|yes|y|1|x|ignore)$/i.test((get("Ignore", 99) ?? "").trim()) || int(get("Priority", 7), 1) < 0,
    });
  }
  return out;
}

/**
 * A talkgroup file as loaded, ready to keep: a Trunk Recorder file with
 * commas comes back as it is; one saved with semicolons or tabs, with a
 * byte-order mark, or with headers in another case is rewritten with commas
 * and Trunk Recorder's header names, every column kept.
 */
export function normalizeTalkgroupCsv(text: string): string {
  const bom = text.startsWith("\uFEFF");
  const body = bom ? text.slice(1) : text;
  const lines = body.split(/\r?\n/);
  const hi = lines.findIndex((l) => l.trim() && !l.trim().startsWith("#"));
  if (hi < 0) return body;
  const delim = csvDelimiter(lines[hi]);
  const header = splitCsvLine(lines[hi], delim);
  const fixed = header.map(canonical);
  const headed = fixed[0] === "Decimal";
  const renamed = headed && fixed.some((h, k) => h !== header[k]);
  if (delim === "," && !renamed) return body;
  return (
    lines
      .map((l, k) => {
        if (!l.trim() || l.trim().startsWith("#")) return l;
        const f = k === hi && headed ? fixed : splitCsvLine(l, delim);
        return f.map(csvCell).join(",");
      })
      .join("\n")
      .replace(/\n*$/, "") + "\n"
  );
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
function row(number: number, f: string[], group: string): Talkgroup {
  let k = 0;
  // The HEX column, when copied (it is the same number).
  if (f[k] && /^[0-9a-f]+$/i.test(f[k]) && Number.parseInt(f[k], 16) === number) k++;
  const m = f[k] ? rrMode(f[k]) : null;
  if (m) k++;
  return { number, mode: m ?? "D", alphaTag: f[k] ?? "", description: f[k + 1] ?? "", tag: f[k + 2] ?? "", group, priority: 1, preferredNac: 0 };
}

/** Talkgroups as Trunk Recorder's headed talkgroup CSV. */
export function talkgroupsToCsv(tgs: Talkgroup[]): string {
  const cell = (v: string) => (/[",\n]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v);
  const rows = tgs.map((t) => [String(t.number), t.number.toString(16), t.alphaTag, t.mode, t.description, t.tag, t.group].map(cell).join(","));
  return ["Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category", ...rows].join("\n") + "\n";
}

const csvCell = (v: string) => (/[",\n]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v);
const LEGACY = ["Decimal", "Hex", "Mode", "Alpha Tag", "Description", "Tag", "Category", "Priority"];

/** The header a headerless, legacy file's columns are (as many as its widest row: 7, or 8 with Priority). */
function legacyHeader(rows: string[], delim = ","): string[] {
  const width = Math.max(...rows.map((l) => splitCsvLine(l, delim).length));
  return LEGACY.slice(0, Math.max(7, Math.min(8, width)));
}

/**
 * A talkgroup file with a header row: a headerless, legacy one is given
 * Trunk Recorder's (Decimal,Hex,Mode,Alpha Tag,Description,Tag,Category
 * [,Priority]); anything else comes back as it is.
 */
export function withHeader(csv: string): string {
  const lines = csv.replace(/^\uFEFF/, "").split(/\r?\n/);
  const data = (l: string) => !!l.trim() && !l.trim().startsWith("#");
  const hi = lines.findIndex(data);
  if (hi < 0) return csv;
  const delim = csvDelimiter(lines[hi]);
  if (canonical(splitCsvLine(lines[hi], delim)[0] ?? "") === "Decimal") return csv;
  lines.splice(hi, 0, legacyHeader(lines.filter(data), delim).join(delim));
  return lines.join("\n");
}

/** A talkgroup file opened for editing in place: its lines, its header's line and columns, its delimiter. */
interface TalkgroupFile {
  lines: string[];
  hi: number;
  /** Its header as written, and as Trunk Recorder names its columns. */
  header: string[];
  cols: string[];
  delim: string;
}

const isData = (l: string) => !!l.trim() && !l.trim().startsWith("#");

/** Open `csv` for editing: a legacy (headerless) file is given its header, none a default one. */
function openFile(csv: string): TalkgroupFile {
  const lines = withHeader(csv).replace(/^\uFEFF/, "").split(/\r?\n/);
  while (lines.length && !lines[lines.length - 1].trim()) lines.pop();
  let hi = lines.findIndex(isData);
  if (hi < 0) {
    lines.push("Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category");
    hi = lines.length - 1;
  }
  const delim = csvDelimiter(lines[hi]);
  const header = splitCsvLine(lines[hi], delim);
  return { lines, hi, header, cols: header.map(canonical), delim };
}

/** A cell in the file's delimiter. */
const cellIn = (delim: string) => (v: string) => (v.includes(delim) || /["\n]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v);

/** Add column `name` to the file (when it has none); its index. */
function ensureColumn(f: TalkgroupFile, name: string): number {
  const at = f.cols.indexOf(name);
  if (at >= 0) return at;
  f.header.push(name);
  f.cols.push(name);
  f.lines[f.hi] = f.header.map(cellIn(f.delim)).join(f.delim);
  return f.cols.length - 1;
}

/** A row for talkgroup `t` in the file's columns (what it doesn't say left empty). */
function rowFor(f: TalkgroupFile, t: Partial<Talkgroup> & { number: number }, more: Record<string, string> = {}): string {
  const v: Record<string, string> = {
    Decimal: String(t.number), Hex: t.number.toString(16), Mode: t.mode || "D", "Alpha Tag": t.alphaTag ?? "", Description: t.description ?? "",
    Tag: t.tag ?? "", Category: t.group ?? "", Priority: t.priority && t.priority > 1 ? String(t.priority) : "", ...more,
  };
  return f.cols.map((c) => cellIn(f.delim)(v[c] ?? "")).join(f.delim);
}

const closeFile = (f: TalkgroupFile) => f.lines.join("\n") + "\n";

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
 * Talkgroup `tg`'s row of a talkgroup file with `cells` set, each [column,
 * value] by Trunk Recorder's column names. Only those cells change, in that
 * row: the file's other rows, comments, columns and delimiter stay as they
 * are. A column it lacks is added; a legacy headerless file is given its
 * header; a talkgroup it lacks gets a row when a value is set. Clearing
 * Ignore also undoes Trunk Recorder's way of ignoring, a priority of −1.
 * The same edit as the recorder's (trunk-core's `edit_rows`), so this copy
 * of the file holds the recorder's talkgroups (the recorder keeps them as
 * rows, and writes them out as `rows_csv` does).
 */
export function withCells(csv: string, tg: number, cells: [string, string][]): string {
  const f = openFile(csv);
  const at = cells.map(([name, v]) => [ensureColumn(f, name), v] as const);
  const pc = f.cols.indexOf("Priority");
  const unignore = cells.some(([n, v]) => n === "Ignore" && !v);
  const cell = cellIn(f.delim);
  let found = false;
  for (let i = f.hi + 1; i < f.lines.length; i++) {
    if (!isData(f.lines[i])) continue;
    const row = splitCsvLine(f.lines[i], f.delim);
    if (!/^\d+$/.test(row[0] ?? "") || Number(row[0]) !== tg) continue;
    found = true;
    while (row.length < f.cols.length) row.push("");
    for (const [c, v] of at) row[c] = v;
    if (unignore && pc >= 0 && /^-\d+$/.test(row[pc])) row[pc] = "1";
    f.lines[i] = row.map(cell).join(f.delim);
  }
  if (!found && cells.some(([, v]) => v)) {
    const v: Record<string, string> = { Decimal: String(tg), Hex: tg.toString(16), Mode: "D" };
    for (const [c, x] of at) v[f.cols[c]] = x;
    f.lines.push(f.cols.map((c) => cell(v[c] ?? "")).join(f.delim));
  }
  return closeFile(f);
}

/**
 * The talkgroup file with talkgroup `tg` marked ignored (never recorded) or
 * not ([`withCells`]); a talkgroup not in the file gets a row, named `alphaTag`.
 */
export function withIgnore(csv: string, tg: number, ignore: boolean, alphaTag = ""): string {
  return withCells(csv, tg, editCells(ignoreEdit(csv, tg, ignore, alphaTag)));
}

/** The edit that marks talkgroup `tg` ignored or not: named `alphaTag` too when the file lacks it. */
export function ignoreEdit(csv: string, tg: number, ignore: boolean, alphaTag = ""): TalkgroupEdit {
  return ignore && alphaTag && !parseTalkgroupCsv(csv).has(tg) ? { alphaTag, ignore } : { ignore };
}

/**
 * `existing` (a system's talkgroup file) with the talkgroups of `incoming` it
 * doesn't have added, in its own columns and delimiter: its rows, names and
 * columns stay as they are (a name of yours is never replaced). No file yet:
 * `incoming` as it is.
 */
export function mergeTalkgroupCsv(existing: string, incoming: string): { csv: string; added: number } {
  const inc = parseTalkgroupCsv(incoming);
  if (!existing.trim()) return { csv: incoming, added: inc.size };
  const have = parseTalkgroupCsv(existing);
  const add = [...inc.values()].filter((t) => !have.has(t.number));
  if (!add.length) return { csv: existing, added: 0 };
  const f = openFile(existing);
  for (const t of add) f.lines.push(rowFor(f, t));
  return { csv: closeFile(f), added: add.length };
}
