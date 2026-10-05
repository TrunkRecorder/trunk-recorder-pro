// Trunk Recorder's filenameFormat, as pieces: tokens, start times, folder
// separators and plain text. The recorder (crates/trunk-app/src/filename.rs)
// takes `/` or `\` as a folder, so a macOS / Linux or a Windows style path
// both work; the pieces are written back with `/`.

/** The call tokens, as filename.rs knows them. */
export const FILENAME_TOKENS = [
  "talkgroup", "talkgroup_tag", "talkgroup_alpha_tag", "talkgroup_description", "talkgroup_group", "talkgroup_display", "short_name", "freq", "freq_mhz",
  "call_num", "tdma_slot", "sys_num", "epoch", "source_num", "recorder_num", "audio_type", "emergency", "encrypted", "priority", "signal", "noise", "color_code", "ran",
];

export type Piece =
  | { kind: "token"; name: string }
  /** `{time:FMT}` (local) or `{ztime:FMT}` (UTC). */
  | { kind: "time"; utc: boolean; fmt: string }
  /** A folder: `/` or `\`. */
  | { kind: "sep" }
  | { kind: "text"; text: string };

/** strftime letters filename.rs knows; `-` after the % drops the padding (%-m). */
const TIME_CODES = "YymdeHIpMSfjaAbhBFTszZ%";

/** Characters a name can't have on Windows (macOS and Linux are fine with most, but a recording folder gets copied around). */
const BAD_CHARS = /[:*?"<>|\u0000-\u001f]/;
/** What plain text may not hold: those, the folder separators and the token braces. */
export const TEXT_NOT_ALLOWED = /[:*?"<>|\u0000-\u001f/\\{}]/g;
const RESERVED = /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(\..*)?$/i;

/** The format as pieces, and what's wrong with it (empty: it's fine). */
export function parseFormat(format: string): { pieces: Piece[]; problems: string[] } {
  const pieces: Piece[] = [];
  const problems: string[] = [];
  const text = (s: string) => {
    const last = pieces[pieces.length - 1];
    if (last?.kind === "text") last.text += s;
    else pieces.push({ kind: "text", text: s });
  };
  for (let i = 0; i < format.length; i++) {
    const ch = format[i];
    if (ch === "/" || ch === "\\") pieces.push({ kind: "sep" });
    else if (ch === "{") {
      const close = format.indexOf("}", i);
      if (close < 0) {
        problems.push("A { has no closing }.");
        text(format.slice(i));
        break;
      }
      const t = format.slice(i + 1, close);
      const time = /^(z?)time:(.*)$/s.exec(t);
      pieces.push(time ? { kind: "time", utc: time[1] === "z", fmt: time[2] } : { kind: "token", name: t });
      i = close;
    } else if (ch === "}") {
      problems.push("A } has no opening {.");
      text(ch);
    } else text(ch);
  }
  problems.push(...pieceProblems(pieces));
  return { pieces, problems: [...new Set(problems)] };
}

/** The pieces as a format, folders written `/`. */
export function formatToString(pieces: Piece[]): string {
  return pieces
    .map((p) => (p.kind === "sep" ? "/" : p.kind === "text" ? p.text : p.kind === "time" ? `{${p.utc ? "ztime" : "time"}:${p.fmt}}` : `{${p.name}}`))
    .join("");
}

/** The problem with one piece on its own, or null. */
export function pieceProblem(p: Piece): string | null {
  if (p.kind === "token" && !FILENAME_TOKENS.includes(p.name)) return `Unknown token {${p.name}}.`;
  if (p.kind === "time") {
    if (!p.fmt) return `{${p.utc ? "ztime" : "time"}:} needs a format, %Y-%m-%d say.`;
    if (p.fmt === "iso" || p.fmt === "iso_ms") return null;
    for (let i = 0; i < p.fmt.length; i++) {
      if (p.fmt[i] !== "%") continue;
      let c = p.fmt[++i];
      if (c === "-") c = p.fmt[++i];
      if (c === undefined) return `A time format can't end with %.`;
      if (!TIME_CODES.includes(c)) return `Unknown time code %${c}.`;
    }
    if (/[*?"<>|]/.test(p.fmt)) return `A time format can't hold * ? " < > |.`;
  }
  if (p.kind === "text") {
    const bad = BAD_CHARS.exec(p.text);
    if (bad) return bad[0] < " " ? "Text can't hold control characters." : `"${bad[0]}" isn't allowed in a file or folder name on Windows.`;
  }
  return null;
}

/** Whole-path problems: where the folders go. */
function pieceProblems(pieces: Piece[]): string[] {
  const out: string[] = [];
  for (const p of pieces) {
    const why = pieceProblem(p);
    if (why) out.push(why);
  }
  if (!pieces.length) return out;
  if (pieces[0].kind === "sep") out.push("It starts with a folder separator, but the path is always inside the recordings folder: leave off the leading /.");
  const first = pieces[0];
  if (first.kind === "text" && /^[A-Za-z]:$/.test(first.text)) out.push("A drive (C:) can't start it: the path is always inside the recordings folder.");
  if (first.kind === "text" && first.text.startsWith("~")) out.push("~ isn't a home folder here: the path is always inside the recordings folder.");
  if (pieces[pieces.length - 1].kind === "sep") out.push("It ends with a folder separator: the file name goes after the last one.");
  // Folder by folder (the last one is the file name, which -call_<number> follows).
  const segments: Piece[][] = [[]];
  for (const p of pieces) {
    if (p.kind === "sep") segments.push([]);
    else segments[segments.length - 1].push(p);
  }
  segments.forEach((seg, k) => {
    const inner = k > 0 && k < segments.length - 1;
    if (!seg.length && inner) out.push("Two folder separators in a row make a folder with no name.");
    const literal = seg.every((p) => p.kind === "text") ? seg.map((p) => (p as { text: string }).text).join("") : null;
    if (literal !== null) {
      if (literal.trim() === "." || literal.trim() === "..") out.push(`A folder can't be named "${literal.trim()}".`);
      else if (RESERVED.test(literal.trim())) out.push(`"${literal.trim()}" is a name Windows reserves.`);
      else if (seg.length && !literal.trim()) out.push("A folder name can't be only spaces.");
    }
    const last = seg[seg.length - 1];
    if (k < segments.length - 1 && last?.kind === "text" && /[ .]$/.test(last.text) && literal?.trim() !== "." && literal?.trim() !== "..")
      out.push(`Windows drops a space or dot at the end of a folder name ("${last.text}").`);
  });
  return out;
}

/** Why a format can't be used, or null. */
export function filenameProblem(format: string): string | null {
  const { problems } = parseFormat(format);
  return problems.length ? problems.join(" ") : null;
}

// ── a preview ────────────────────────────────────────────────────────────────

/** A made-up call to show a format with. */
export const SAMPLE_CALL = {
  talkgroup: 1201,
  talkgroup_tag: "Fire Dispatch",
  talkgroup_alpha_tag: "FD Disp",
  talkgroup_description: "Fire / EMS Dispatch",
  talkgroup_group: "Fire",
  short_name: "county",
  freq: 851012500,
  call_num: 42,
  tdma_slot: "",
  sys_num: 0,
  source_num: 0,
  recorder_num: 3,
  audio_type: "digital",
  emergency: 0,
  encrypted: 0,
  priority: 1,
  signal: -48,
  noise: -112,
  color_code: -1,
  ran: -1,
};

const DAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/** strftime as filename.rs does it, for `d` (local, or UTC). */
function strftime(fmt: string, d: Date, utc: boolean): string {
  if (fmt === "iso") return strftime("%Y-%m-%dT%H:%M:%S", d, utc) + (utc ? "Z" : "");
  if (fmt === "iso_ms") return strftime("%Y-%m-%dT%H:%M:%S.%f", d, utc) + (utc ? "Z" : "");
  const g = utc
    ? { y: d.getUTCFullYear(), mo: d.getUTCMonth(), d: d.getUTCDate(), h: d.getUTCHours(), mi: d.getUTCMinutes(), s: d.getUTCSeconds(), ms: d.getUTCMilliseconds(), wd: d.getUTCDay() }
    : { y: d.getFullYear(), mo: d.getMonth(), d: d.getDate(), h: d.getHours(), mi: d.getMinutes(), s: d.getSeconds(), ms: d.getMilliseconds(), wd: d.getDay() };
  const jan1 = utc ? Date.UTC(g.y, 0, 1) : new Date(g.y, 0, 1).getTime();
  const yday = Math.floor(((utc ? Date.UTC(g.y, g.mo, g.d) : new Date(g.y, g.mo, g.d).getTime()) - jan1) / 86_400_000) + 1;
  const off = utc ? 0 : -d.getTimezoneOffset();
  let out = "";
  for (let i = 0; i < fmt.length; i++) {
    if (fmt[i] !== "%") {
      out += fmt[i];
      continue;
    }
    let c = fmt[++i];
    const bare = c === "-";
    if (bare) c = fmt[++i];
    const p = (v: number, n = 2) => (bare ? String(v) : String(v).padStart(n, "0"));
    const v: Record<string, () => string> = {
      Y: () => String(g.y),
      y: () => p(g.y % 100),
      m: () => p(g.mo + 1),
      d: () => p(g.d),
      e: () => (bare ? String(g.d) : String(g.d).padStart(2, " ")),
      H: () => p(g.h),
      I: () => p(g.h % 12 || 12),
      p: () => (g.h < 12 ? "AM" : "PM"),
      M: () => p(g.mi),
      S: () => p(g.s),
      f: () => p(g.ms, 3),
      j: () => p(yday, 3),
      a: () => DAYS[g.wd].slice(0, 3),
      A: () => DAYS[g.wd],
      b: () => MONTHS[g.mo].slice(0, 3),
      h: () => MONTHS[g.mo].slice(0, 3),
      B: () => MONTHS[g.mo],
      F: () => `${g.y}-${p(g.mo + 1)}-${p(g.d)}`,
      T: () => `${p(g.h)}:${p(g.mi)}:${p(g.s)}`,
      s: () => String(Math.floor(d.getTime() / 1000)),
      z: () => `${off < 0 ? "-" : "+"}${String(Math.floor(Math.abs(off) / 60)).padStart(2, "0")}${String(Math.abs(off) % 60).padStart(2, "0")}`,
      Z: () => (utc ? "UTC" : ""),
      "%": () => "%",
    };
    out += c === undefined ? "%" : (v[c]?.() ?? `%${bare ? "-" : ""}${c}`);
  }
  return out;
}

const clean = (s: string) => s.trim().replace(/[\\/:*?"<>|\s]/g, "_");
const cleanTime = (s: string) => s.replace(/:/g, "-").replace(/[\\*?"<>|]/g, "_");

/** One piece of the sample call, as the recorder would write it. */
export function pieceSample(p: Piece, when: Date): string {
  if (p.kind === "sep") return "/";
  if (p.kind === "text") return p.text;
  if (p.kind === "time") return cleanTime(strftime(p.fmt, when, p.utc));
  const c = SAMPLE_CALL as Record<string, string | number>;
  switch (p.name) {
    case "talkgroup":
    case "talkgroup_display":
      return String(c.talkgroup);
    case "freq_mhz":
      return (c.freq as number / 1e6).toFixed(4);
    case "epoch":
      return String(Math.floor(when.getTime() / 1000));
    default:
      return p.name in c ? (typeof c[p.name] === "string" ? clean(c[p.name] as string) : String(c[p.name])) : `{${p.name}}`;
  }
}

/** Where the sample call would go: its folders and its file name, as filename.rs tidies them. */
export function samplePath(pieces: Piece[], when: Date): string[] {
  const path = pieces.map((p) => pieceSample(p, when)).join("");
  const parts = path.split(/[/\\]/).map((s) => s.trim()).filter((s) => s && s !== "." && s !== "..");
  if (!parts.length) parts.push("call");
  parts[parts.length - 1] += `-call_${SAMPLE_CALL.call_num}`;
  return parts;
}
