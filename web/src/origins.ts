// Where a talkgroup's or radio's names came from (protocol.ts Origin): the
// last change to its row, as the recorder keeps it (crates/trunk-core/src/trunk/names.rs).

import type { Origin, OriginFrom } from "./protocol.ts";

/** Typed in now. */
export function editedNow(): Origin {
  return { kind: "edited", at: Math.floor(Date.now() / 1000) };
}

/** `from` now: what the recorder makes of a list sent from it. */
export function fromNow(from: OriginFrom | undefined): Origin | undefined {
  return from && { ...from, at: Math.floor(Date.now() / 1000) };
}

/** What each kind is called, said of a row's names. */
const SAID: Record<Origin["kind"], string> = {
  file: "Loaded from",
  radioReference: "Copied from RadioReference",
  directory: "From the directory",
  trunkRecorder: "From Trunk Recorder's",
  edited: "Edited by hand",
};

/** Where a row's names came from, in words ("Loaded from dc.csv"); none: not known. */
export function originText(o: Origin | undefined): string | undefined {
  if (!o) return undefined;
  const said = SAID[o.kind];
  if (o.kind === "file" || o.kind === "trunkRecorder") return o.name ? `${said} ${o.name}` : o.kind === "file" ? "Loaded from a file" : "From a Trunk Recorder config";
  return o.name && o.kind === "directory" ? `${said} (${o.name})` : said;
}

/** How many of `rows` came each way (kinds with none left out), most first. */
export function originCounts(rows: { origin?: Origin }[]): [Origin["kind"], number][] {
  const n = new Map<Origin["kind"], number>();
  for (const r of rows) if (r.origin) n.set(r.origin.kind, (n.get(r.origin.kind) ?? 0) + 1);
  return [...n].sort((a, b) => b[1] - a[1]);
}

/** A kind's short name, for counts ("212 RadioReference · 3 edited"). */
export const ORIGIN_SHORT: Record<Origin["kind"], string> = {
  file: "from a file",
  radioReference: "RadioReference",
  directory: "directory",
  trunkRecorder: "Trunk Recorder",
  edited: "edited",
};
