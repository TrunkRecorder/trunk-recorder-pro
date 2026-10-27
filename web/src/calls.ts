// A page of calls from a list held here (the browser build's call index):
// the same filter and paging as the desktop recorder's call store
// (trunk-pro db/read.rs `page_of`): newest first, equal start times never
// split between pages, so `before` loses and repeats nothing.
import type { CallEntry, ToRecorder } from "./protocol.ts";

export type CallsQuery = Omit<Extract<ToRecorder, { type: "callsQuery" }>, "type" | "id">;

export interface CallsPage {
  entries: CallEntry[];
  more: boolean;
  before: number;
}

const startOf = (c: CallEntry) => c.record.start_time_ms ?? 0;

/** Whether call `c` is one `q` wants: its system, talkgroup, radio and time. */
export function wanted(c: CallEntry, q: CallsQuery): boolean {
  const t = startOf(c);
  return (
    (q.system === undefined || c.record.short_name === q.system) &&
    (q.talkgroup === undefined || c.record.talkgroup === q.talkgroup) &&
    (q.unit === undefined || (c.record.srcList ?? []).some((s) => s.src === q.unit)) &&
    (q.from === undefined || t >= q.from) &&
    (q.to === undefined || t < q.to) &&
    (q.before === undefined || t < q.before)
  );
}

/** The page of `calls` that `q` asks for (`limit` 1 to 500, 100 by default). */
export function callsPage(calls: CallEntry[], q: CallsQuery): CallsPage {
  const v = calls.filter((c) => wanted(c, q)).sort((a, b) => startOf(b) - startOf(a) || (a.path < b.path ? 1 : a.path > b.path ? -1 : 0));
  const limit = Math.min(500, Math.max(1, q.limit ?? 100));
  let take = Math.min(v.length, limit);
  while (take > 0 && take < v.length && startOf(v[take]) === startOf(v[take - 1])) take++;
  const entries = v.slice(0, take);
  return { entries, more: take < v.length, before: entries.length ? startOf(entries[entries.length - 1]) : (q.before ?? 0) };
}
