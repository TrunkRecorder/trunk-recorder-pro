// CTCSS tones and DCS codes as people write them on a conventional channel.
// The same rules as the recorder's (crates/trunk-core/src/dsp/tones.rs:
// Tone::parse, Tone::matches) — keep them together.

/** The CTCSS tones, tenths of a hertz. */
export const CTCSS = [
  670, 693, 719, 744, 770, 797, 825, 854, 885, 915, 948, 974, 1000, 1035, 1072, 1109, 1148, 1188, 1230, 1273, 1318, 1365, 1413, 1462, 1500, 1514, 1567,
  1598, 1622, 1655, 1679, 1713, 1738, 1773, 1799, 1835, 1862, 1899, 1928, 1966, 1995, 2035, 2065, 2107, 2181, 2257, 2291, 2336, 2418, 2503, 2541,
];

/** The DCS codes, their octal digits read as a decimal number (D023 → 23). */
export const DCS = [
  6, 7, 15, 17, 21, 23, 25, 26, 27, 31, 32, 36, 43, 47, 50, 51, 53, 54, 65, 71, 72, 73, 74, 114, 115, 116, 122, 125, 131, 132, 134, 143, 145, 152, 155,
  156, 162, 165, 172, 174, 205, 212, 223, 225, 226, 232, 243, 244, 245, 246, 251, 252, 255, 261, 263, 265, 266, 271, 274, 306, 311, 315, 325, 331, 332,
  343, 346, 351, 356, 364, 365, 371, 411, 412, 413, 423, 431, 432, 445, 446, 452, 454, 455, 462, 464, 465, 466, 503, 506, 516, 523, 526, 532, 546, 565,
  606, 612, 624, 627, 631, 632, 654, 662, 664, 703, 712, 723, 731, 732, 734, 743, 754,
];

const NONE = ["", "0", "0.0", "S", "SEARCH", "CSQ", "NONE", "OFF", "ANY"];
const DCS_WORDS = ["DPL", "DCS", "CDCSS", "DTCS"];
const DROP = ["PL", "TPL", "CTCSS", "CTC", "HZ", "TONE", ...DCS_WORDS];

const ctcssText = (t: number) => `${Math.floor(t / 10)}.${t % 10}`;
const ctcssNear = (hz: number) => CTCSS.find((t) => Math.abs(t / 10 - hz) < 0.05);

/**
 * A channel's Tone in Trunk Recorder's form (`151.4`, `D023N`) from what was
 * typed (`151.4 PL`, `PL 151.4`, `023 DPL`, `D023`, `023`, `D023I` …);
 * `tone: ""` for none (empty, 0, S, CSQ), or why it can't be read.
 */
export function parseTone(text: string): { tone: string } | { error: string } {
  const up = text.trim().toUpperCase();
  if (NONE.includes(up)) return { tone: "" };
  const words = up.split(/[\s,]+/).filter(Boolean);
  const dcsWord = words.some((w) => DCS_WORDS.includes(w));
  let body = words.filter((w) => !DROP.includes(w)).join("");
  if (body.endsWith("HZ")) body = body.slice(0, -2);
  const bad = { error: `"${text.trim()}" isn't a CTCSS tone (67.0–254.1 Hz) or a DCS code (like D023N)` };
  const b = body.startsWith("D") ? body.slice(1) : body;
  const last = b.slice(-1);
  const inverted = last === "I" || last === "R";
  const digits = last === "N" || inverted ? b.slice(0, -1) : b;
  const octal = /^[0-7]{1,3}$/.test(digits);
  const marked = body.startsWith("D") || dcsWord || digits.length !== body.length;
  if (marked || (octal && !body.includes(".") && ctcssNear(Number(body)) === undefined)) {
    if (!octal) return bad;
    const code = Number(digits);
    const name = `D${String(code).padStart(3, "0")}`;
    return DCS.includes(code) ? { tone: `${name}${inverted ? "I" : "N"}` } : { error: `${name} isn't a DCS code` };
  }
  const hz = Number(body);
  if (!body || !Number.isFinite(hz)) return bad;
  const t = ctcssNear(hz);
  if (t !== undefined) return { tone: ctcssText(t) };
  const nearest = CTCSS.reduce((a, c) => (Math.abs(c / 10 - hz) < Math.abs(a / 10 - hz) ? c : a));
  return Math.abs(nearest / 10 - hz) <= 3 ? { error: `${hz} Hz isn't a standard CTCSS tone — ${ctcssText(nearest)}?` } : bad;
}

/** A DCS code's 23 bits on the air (first sent in bit 22): Golay (23,12), generator 0xC75. */
function dcsPattern(code: number, inverted: boolean): number {
  const data = (Math.floor(code / 100) % 10 << 6) | (Math.floor(code / 10) % 10 << 3) | code % 10;
  const msg = (0b100 << 9) | data;
  let reg = msg << 11;
  for (let i = 22; i >= 11; i--) if ((reg >> i) & 1) reg ^= 0xc75 << (i - 11);
  let p = 0;
  for (let i = 0; i < 12; i++) p |= ((msg >> i) & 1) << (22 - i);
  for (let i = 0; i < 11; i++) p |= ((reg >> i) & 1) << (10 - i);
  return inverted ? ~p & 0x7fffff : p;
}

/** Two tones (Trunk Recorder's form) are the same signal: equal, or DCS codes whose words are rotations of each other (D023N = D047I). */
export function sameTone(a: string, b: string): boolean {
  if (a === b) return true;
  const m = (t: string) => /^D(\d{3})([NI])$/.exec(t);
  const x = m(a);
  const y = m(b);
  if (!x || !y) return false;
  const pa = dcsPattern(Number(x[1]), x[2] === "I");
  const pb = dcsPattern(Number(y[1]), y[2] === "I");
  for (let r = 0; r < 23; r++) if ((((pa << r) | (pa >>> (23 - r))) & 0x7fffff) === pb) return true;
  return false;
}

/**
 * A row's Tone column for its mode, in the recorder's form (crates/trunk-core
 * src/trunk/conventional.rs: Access::parse — keep them together): FM as
 * parseTone; P25's NAC (`293`, `293 NAC`, `$293`, `0x293` → `NAC 293`; `F7E`
 * / `F7F` = any); DMR's colour code, slot and talkgroup (`CC1`, `1`,
 * `CC1 TS2 TG201`, `CC 1 TG 201 SL 2` → `CC 1 TS 2 TG 201`); NXDN's RAN
 * and group (`RAN 5`, `5`, `ran5 tg 201` → `RAN 5 TG 201`; RAN 0 = any).
 */
export function parseAccess(mode: "fm" | "p25" | "dmr" | "nxdn48" | "nxdn96", text: string): { tone: string } | { error: string } {
  if (mode === "fm") return parseTone(text);
  const up = text.trim().toUpperCase();
  if (["", "0", "S", "ANY", "NONE", "SEARCH"].includes(up)) return { tone: "" };
  if (mode === "p25") {
    const bad = { error: `"${text.trim()}" isn't a NAC (up to 3 hex digits, like 293)` };
    let body = up
      .split(/\s+/)
      .filter((w) => w && w !== "NAC")
      .join("")
      .replace(/^\$+/, "");
    if (body.startsWith("0X")) body = body.slice(2);
    if (!/^[0-9A-F]{1,3}$/.test(body)) return bad;
    const n = parseInt(body, 16);
    return n === 0xf7e || n === 0xf7f ? { tone: "" } : { tone: `NAC ${n.toString(16).toUpperCase().padStart(3, "0")}` };
  }
  const nxdn = mode === "nxdn48" || mode === "nxdn96";
  const bad = {
    error: nxdn
      ? `"${text.trim()}" isn't an NXDN RAN (RAN 5) or talkgroup (TG 201)`
      : `"${text.trim()}" isn't a DMR colour code (CC1), slot (TS2) or talkgroup (TG201)`,
  };
  const words = up
    .replace(/[,:=]/g, " ")
    .replace(/([A-Z])(\d)/g, "$1 $2")
    .replace(/(\d)([A-Z])/g, "$1 $2")
    .split(/\s+/)
    .filter(Boolean);
  let cc: number | undefined;
  let slot: number | undefined;
  let tg: number | undefined;
  let ran: number | undefined;
  for (let i = 0; i < words.length; ) {
    const bare = /^\d+$/.test(words[i]);
    const key = bare && i === 0 ? (nxdn ? "RAN" : "CC") : words[i];
    const valText = bare ? words[i] : words[i + 1];
    if (!valText || !/^\d+$/.test(valText)) return bad;
    const v = Number(valText);
    i += bare ? 1 : 2;
    if (nxdn) {
      if (key === "RAN") {
        if (v > 63) return { error: `RAN ${v}: a RAN is 0–63` };
        ran = v;
      } else if (key === "TG") {
        if (v < 1 || v >= 1 << 16) return { error: `TG ${v}: not an NXDN group (1–65535)` };
        tg = v;
      } else return bad;
    } else if (key === "CC" || key === "COLOR" || key === "COLOUR") {
      if (v > 15) return { error: `CC ${v}: a colour code is 0–15` };
      cc = v;
    } else if (key === "TS" || key === "SLOT") {
      if (v < 1 || v > 2) return { error: `TS ${v}: the slot is 1 or 2` };
      slot = v;
    } else if (key === "TG") {
      if (v < 1 || v >= 1 << 24) return { error: `TG ${v}: not a DMR talkgroup` };
      tg = v;
    } else if (key !== "SL") return bad;
  }
  if (nxdn) {
    // RAN 0 means "any" to a receiver.
    const parts = [ran ? `RAN ${ran}` : "", tg !== undefined ? `TG ${tg}` : ""].filter(Boolean);
    return { tone: parts.join(" ") };
  }
  const parts = [cc !== undefined ? `CC ${cc}` : "", slot !== undefined ? `TS ${slot}` : "", tg !== undefined ? `TG ${tg}` : ""].filter(Boolean);
  return { tone: parts.join(" ") };
}

/** The talkgroup a DMR or NXDN row's code names, if any. */
export function dmrTalkgroup(tone: string | undefined): number | undefined {
  const m = /\bTG (\d+)/.exec(tone ?? "");
  return m ? Number(m[1]) : undefined;
}
