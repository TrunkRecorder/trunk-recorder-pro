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
    });
  }
  return out;
}
