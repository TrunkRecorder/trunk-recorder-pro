// Trunk Recorder talkgroup CSV (trunk-recorder/talkgroups.cc): either the
// headed format (first column "Decimal"; any of Decimal, Hex, Mode, Alpha Tag,
// Description, Tag, Category, Priority, Preferred NAC, Comment in any order) or
// the legacy headerless one: Decimal,Hex,Mode,Alpha Tag,Description,Tag,Group[,Priority].

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

/** RFC-4180-ish line split: commas, double-quoted fields, "" escapes. */
export function splitCsvLine(line: string): string[] {
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
    else if (ch === ",") {
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

export function parseTalkgroupCsv(text: string): Map<number, Talkgroup> {
  const lines = text.split(/\r?\n/).filter((l) => l.trim() && !l.trim().startsWith("#"));
  const out = new Map<number, Talkgroup>();
  if (!lines.length) return out;
  const first = splitCsvLine(lines[0]);
  const headed = first[0] === "Decimal";
  const col = (name: string) => first.indexOf(name);
  const rows = headed ? lines.slice(1) : lines;
  for (const line of rows) {
    const f = splitCsvLine(line);
    const get = (name: string, legacy: number) => (headed ? (col(name) >= 0 ? f[col(name)] : undefined) : f[legacy]);
    const number = int(get("Decimal", 0), NaN);
    if (!Number.isFinite(number)) continue;
    out.set(number, {
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

/** Mode letters RadioReference's talkgroup tables use (Trunk Recorder's too). */
const RR_MODE = /^(A|D|T|E|M|DE|TE|DM|AE)$/i;

/**
 * A Mode cell: "D", or the letter with RadioReference's encryption badge
 * after it ("D ENC") — Trunk Recorder's encrypted modes (DE / TE / E), which
 * it doesn't record. Null when it isn't a mode.
 */
function rrMode(cell: string): string | null {
  const [base = "", ...badges] = cell.split(/\s+/);
  if (!RR_MODE.test(base)) return null;
  const mode = base.toUpperCase();
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
