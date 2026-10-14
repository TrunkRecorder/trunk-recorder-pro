// The Playback page: search the recordings folder (any time back, not just
// the Listen page's last 24 hours) by system, talkgroup, radio and name, and
// play what turns up, one call after another if wanted. The recorder does the
// searching (playbackQuery); this keeps the filters, the answer and the player.
// Playback lives here, not in the page, so it goes on in other tabs.

import { useRef, useSyncExternalStore } from "react";
import { onRecorderMessage, setListen, shallowEqual, snapshot, transport } from "./controller.ts";
import type { CallEntry, PlaybackTalkgroup, PlaybackUnit } from "./protocol.ts";

export type Span = "1h" | "24h" | "7d" | "30d" | "all";
export const SPANS: { id: Span; label: string; ms: number }[] = [
  { id: "1h", label: "Last hour", ms: 3600_000 },
  { id: "24h", label: "24 hours", ms: 86_400_000 },
  { id: "7d", label: "7 days", ms: 7 * 86_400_000 },
  { id: "30d", label: "30 days", ms: 30 * 86_400_000 },
  { id: "all", label: "Everything", ms: 10 * 365 * 86_400_000 },
];

export interface Filters {
  system: string | null;
  talkgroup: number | null;
  unit: number | null;
  text: string;
  /** Only talkgroups the system's talkgroup file lacks. */
  newOnly: boolean;
  span: Span;
}

export interface PlaybackState {
  filters: Filters;
  /** Newest first. */
  calls: CallEntry[];
  total: number;
  more: boolean;
  truncated: boolean;
  searched: boolean;
  loading: boolean;
  systems: string[];
  talkgroups: PlaybackTalkgroup[];
  units: PlaybackUnit[];
  playing: CallEntry | null;
  paused: boolean;
  position: number;
  duration: number;
  /** Go on to the next call in the list when one ends. */
  autoplay: boolean;
  /** "Next" is the newer call (a conversation in order), else the one below. */
  chrono: boolean;
}

export const NO_FILTERS: Filters = { system: null, talkgroup: null, unit: null, text: "", newOnly: false, span: "7d" };

let state: PlaybackState = {
  filters: NO_FILTERS,
  calls: [],
  total: 0,
  more: false,
  truncated: false,
  searched: false,
  loading: false,
  systems: [],
  talkgroups: [],
  units: [],
  playing: null,
  paused: false,
  position: 0,
  duration: 0,
  autoplay: true,
  chrono: false,
};

const listeners = new Set<() => void>();
function set(patch: Partial<PlaybackState>): void {
  state = { ...state, ...patch };
  for (const l of listeners) l();
}

export function usePlayback<T>(pick: (s: PlaybackState) => T): T {
  const last = useRef<{ s: PlaybackState; v: T } | null>(null);
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => {
      const l = last.current;
      if (l?.s === state) return l.v;
      const v = pick(state);
      last.current = { s: state, v: l && shallowEqual(l.v, v) ? l.v : v };
      return last.current.v;
    },
  );
}

// ── searching ────────────────────────────────────────────────────────────────

const PAGE = 100;
let queryId = 0;

/** The request for `f`, from `before` (the next page) or the newest. */
export function requestFor(f: Filters, now: number, id: number, before?: number) {
  const span = SPANS.find((x) => x.id === f.span) ?? SPANS[2];
  return {
    type: "playbackQuery" as const,
    id,
    ...(f.system ? { system: f.system } : {}),
    ...(f.talkgroup !== null ? { talkgroup: f.talkgroup } : {}),
    ...(f.unit !== null ? { unit: f.unit } : {}),
    ...(f.text.trim() ? { text: f.text.trim() } : {}),
    ...(f.newOnly ? { newOnly: true } : {}),
    from: now - span.ms,
    to: now + 60_000,
    limit: PAGE,
    ...(before !== undefined ? { before } : {}),
  };
}

/** Search again with these filters changed. */
export function setFilters(patch: Partial<Filters>): void {
  set({ filters: { ...state.filters, ...patch } });
  search();
}

export function clearFilters(): void {
  set({ filters: { ...NO_FILTERS, span: state.filters.span } });
  search();
}

/** Run the search (the first page). */
export function search(): void {
  const id = ++queryId;
  set({ loading: true });
  transport.send(requestFor(state.filters, Date.now(), id));
}

/** The next page of the search, older calls. */
export function loadMore(): void {
  const oldest = state.calls.at(-1)?.record.start_time_ms;
  if (state.loading || oldest === undefined) return;
  set({ loading: true });
  transport.send(requestFor(state.filters, Date.now(), ++queryId, oldest));
}

onRecorderMessage((m) => {
  if (m.type !== "playbackResult" || m.id !== queryId) return;
  const first = m.before === null;
  const seen = new Set(state.calls.map((c) => c.path));
  set({
    calls: first ? m.entries : [...state.calls, ...m.entries.filter((c) => !seen.has(c.path))],
    total: first ? m.total : state.total,
    more: m.more,
    truncated: first ? m.truncated : state.truncated || m.truncated,
    searched: true,
    loading: false,
    systems: m.systems.length || first ? m.systems : state.systems,
    talkgroups: m.talkgroups ?? state.talkgroups,
    units: m.units ?? state.units,
  });
});

// ── playing ──────────────────────────────────────────────────────────────────

/** Whether a call can be played: it has audio and isn't encrypted. */
export const playable = (c: CallEntry) => c.audio !== false && !c.record.encrypted;

const audio = typeof Audio === "undefined" ? null : new Audio();
let token = 0;

export async function play(c: CallEntry): Promise<void> {
  if (!audio) return;
  const mine = ++token;
  // One sound at a time: not the scanner, not live audio.
  stopOthers?.();
  if (snapshot().listen) setListen(false);
  set({ playing: c, paused: false, position: 0, duration: c.record.call_length_ms / 1000 });
  let url: string;
  try {
    url = await transport.callUrl(c.path, "m4a");
  } catch {
    if (mine === token) next();
    return;
  }
  if (mine !== token) return;
  audio.src = url;
  audio.play().catch((e: unknown) => {
    if (mine === token && e instanceof DOMException && e.name === "NotAllowedError") set({ paused: true });
  });
}

/** Set by the Listen module (it imports this one): stops its scanner when we play. */
let stopOthers: (() => void) | null = null;
export function onPlay(f: () => void): void {
  stopOthers = f;
}

/** The call after `c` in the list, the way "next" goes; null at the end. */
function neighbour(c: CallEntry, step: 1 | -1): CallEntry | null {
  const i = state.calls.findIndex((x) => x.path === c.path);
  if (i < 0) return null;
  const dir = state.chrono ? -step : step;
  for (let j = i + dir; j >= 0 && j < state.calls.length; j += dir) if (playable(state.calls[j])) return state.calls[j];
  return null;
}

/** The current call is over: the next one if autoplay, else stop. */
function next(): void {
  const was = state.playing;
  const n = was && state.autoplay ? neighbour(was, 1) : null;
  if (n) void play(n);
  else {
    token++;
    set({ playing: null, paused: false, position: 0, duration: 0 });
  }
}

export const skipNext = () => {
  audio?.pause();
  next();
};

export function skipBack(): void {
  const c = state.playing;
  if (!c || !audio) return;
  // Within the first seconds: the previous call; else this one from the start.
  const p = audio.currentTime < 3 ? neighbour(c, -1) : null;
  if (p) void play(p);
  else audio.currentTime = 0;
}

export function togglePause(): void {
  if (!audio || !state.playing) return;
  if (state.paused) {
    set({ paused: false });
    audio.play().catch(() => set({ paused: true }));
  } else {
    set({ paused: true });
    audio.pause();
  }
}

export function stopPlayback(): void {
  if (!state.playing) return;
  token++;
  audio?.pause();
  set({ playing: null, paused: false, position: 0, duration: 0 });
}

export function seek(s: number): void {
  if (audio && Number.isFinite(s)) audio.currentTime = s;
}

export const setAutoplay = (autoplay: boolean) => set({ autoplay });
export const setChrono = (chrono: boolean) => set({ chrono });

if (audio) {
  audio.onended = next;
  audio.onerror = () => {
    if (state.playing) next();
  };
  audio.ontimeupdate = () => set({ position: audio.currentTime });
  audio.ondurationchange = () => Number.isFinite(audio.duration) && set({ duration: audio.duration });
}
