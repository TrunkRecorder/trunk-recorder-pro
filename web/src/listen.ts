// The Listen page's scanner: recorded calls played one after another as they
// conclude (the live feed), filtered by the systems and talkgroups chosen,
// with hold, avoid, pause, replay and skip. It all happens in this page: the
// recorder only sends each call as it concludes, and older ones when asked.
// Playback lives here, not in the page, so it goes on in other tabs.

import { useRef, useSyncExternalStore } from "react";
import { onRecorderMessage, setListen, shallowEqual, snapshot, subscribe, transport } from "./controller.ts";
import type { CallEntry } from "./protocol.ts";

/** What to hear, kept in this browser. Talkgroups are `<system>:<talkgroup>`. */
export interface Selection {
  /** Systems switched off. */
  offSystems: string[];
  /** Talkgroups switched off (any not here is on, new ones too). */
  offTalkgroups: string[];
  /** Only this system (talkgroup null), or this talkgroup, for now. */
  hold: { system: string; talkgroup: number | null } | null;
  /** Talkgroups avoided: until when (Unix ms; 0 = until let go). */
  avoid: Record<string, number>;
}

export interface ListenState {
  /** The live feed: each new call that's wanted is queued and played. */
  feed: boolean;
  paused: boolean;
  playing: CallEntry | null;
  /** Its progress (s) and length (s). */
  position: number;
  duration: number;
  queue: CallEntry[];
  /** Played, newest first (the last few). */
  played: CallEntry[];
  /** Calls dropped from a full queue. */
  missed: number;
  /** Every call known, newest first: as they conclude, and older ones loaded. */
  calls: CallEntry[];
  /** Loading older calls; whether there are older ones still (null: not asked yet). */
  loadingOlder: boolean;
  moreOlder: boolean | null;
  sel: Selection;
}

const SEL_KEY = "trp.listen";
const QUEUE_MAX = 50;
const PLAYED_MAX = 5;
const CALLS_MAX = 5000;
export const OLDER_PAGE = 200;

function loadSelection(): Selection {
  const none: Selection = { offSystems: [], offTalkgroups: [], hold: null, avoid: {} };
  try {
    const v = JSON.parse(localStorage.getItem(SEL_KEY) ?? "null") as Partial<Selection> | null;
    if (!v || typeof v !== "object") return none;
    return {
      offSystems: Array.isArray(v.offSystems) ? v.offSystems.filter((x) => typeof x === "string") : [],
      offTalkgroups: Array.isArray(v.offTalkgroups) ? v.offTalkgroups.filter((x) => typeof x === "string") : [],
      hold: v.hold && typeof v.hold.system === "string" ? { system: v.hold.system, talkgroup: typeof v.hold.talkgroup === "number" ? v.hold.talkgroup : null } : null,
      avoid: v.avoid && typeof v.avoid === "object" ? v.avoid : {},
    };
  } catch {
    return none;
  }
}

let state: ListenState = {
  feed: false,
  paused: false,
  playing: null,
  position: 0,
  duration: 0,
  queue: [],
  played: [],
  missed: 0,
  calls: [],
  loadingOlder: false,
  moreOlder: null,
  sel: loadSelection(),
};

const listeners = new Set<() => void>();
function set(patch: Partial<ListenState>): void {
  state = { ...state, ...patch };
  for (const l of listeners) l();
}

/** One slice of the scanner's state (re-renders when it changes, shallowly). */
export function useListen<T>(pick: (s: ListenState) => T): T {
  const last = useRef<{ s: ListenState; v: T } | null>(null);
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

// ── which calls ──────────────────────────────────────────────────────────────

/** A recorded call's system: its record's short_name, else its folder. */
export const systemOf = (c: CallEntry) => c.record.short_name || c.path.split("/")[0] || "";
export const tgKey = (system: string, talkgroup: number) => `${system}:${talkgroup}`;

/** Its talkgroup and any patched with it. */
const talkgroupsOf = (c: CallEntry) => [c.record.talkgroup, ...(c.record.patched_talkgroups ?? [])];

/** Whether a call can be played at all: it has audio, and it's not encrypted. */
export const playable = (c: CallEntry) => c.audio !== false && !c.record.encrypted;

export function avoided(sel: Selection, key: string, now = Date.now()): boolean {
  const until = sel.avoid[key];
  return until !== undefined && (until === 0 || until > now);
}

/** Whether the scanner plays this call (held, or selected and not avoided). */
export function wanted(sel: Selection, c: CallEntry, now = Date.now()): boolean {
  if (!playable(c)) return false;
  const system = systemOf(c);
  if (sel.hold) return sel.hold.system === system && (sel.hold.talkgroup === null || talkgroupsOf(c).includes(sel.hold.talkgroup));
  if (sel.offSystems.includes(system)) return false;
  return talkgroupsOf(c).some((tg) => {
    const k = tgKey(system, tg);
    return !sel.offTalkgroups.includes(k) && !avoided(sel, k, now);
  });
}

function setSelection(sel: Selection): void {
  try {
    localStorage.setItem(SEL_KEY, JSON.stringify(sel));
  } catch {
    /* not kept; still used this session */
  }
  // What's queued follows the selection.
  set({ sel, queue: state.queue.filter((c) => wanted(sel, c)) });
}

/** Switch a system on or off. */
export function setSystemOn(system: string, on: boolean): void {
  const off = state.sel.offSystems.filter((x) => x !== system);
  setSelection({ ...state.sel, offSystems: on ? off : [...off, system] });
}

/** Switch talkgroups (keys) on or off, together. */
export function setTalkgroupsOn(keys: string[], on: boolean): void {
  const drop = new Set(keys);
  const off = state.sel.offTalkgroups.filter((x) => !drop.has(x));
  setSelection({ ...state.sel, offTalkgroups: on ? off : [...off, ...keys] });
}

/** Everything on (or every listed system and talkgroup off). */
export function setAll(on: boolean, systems: string[], keys: string[]): void {
  setSelection({ ...state.sel, offSystems: on ? [] : systems, offTalkgroups: on ? [] : keys });
}

/** The call the buttons act on: the one playing, else the last played. */
export const subject = (s: ListenState): CallEntry | null => s.playing ?? s.played[0] ?? null;

export function holdSystem(): void {
  const c = subject(state);
  const h = state.sel.hold;
  if (h && h.talkgroup === null) return setSelection({ ...state.sel, hold: null });
  if (c) setSelection({ ...state.sel, hold: { system: systemOf(c), talkgroup: null } });
}

export function holdTalkgroup(): void {
  const c = subject(state);
  const h = state.sel.hold;
  if (h && h.talkgroup !== null) return setSelection({ ...state.sel, hold: null });
  if (c) setSelection({ ...state.sel, hold: { system: systemOf(c), talkgroup: c.record.talkgroup } });
}

/** Avoid the current talkgroup for `minutes` (0: until let go, and again: let it go). */
export function avoid(minutes: number): void {
  const c = subject(state);
  if (!c) return;
  const k = tgKey(systemOf(c), c.record.talkgroup);
  const now = Date.now();
  const { [k]: _, ...rest } = state.sel.avoid;
  // Expired avoids go too.
  for (const [key, until] of Object.entries(rest)) if (until && until <= now) delete rest[key];
  // AVOID again lets it go; a timed one (again) starts its time over.
  const again = minutes === 0 && avoided(state.sel, k, now);
  setSelection({ ...state.sel, avoid: again ? rest : { ...rest, [k]: minutes ? now + minutes * 60_000 : 0 } });
  if (!again && state.playing && tgKey(systemOf(state.playing), state.playing.record.talkgroup) === k) skip();
}

export function letGo(key: string): void {
  const { [key]: _, ...rest } = state.sel.avoid;
  setSelection({ ...state.sel, avoid: rest });
}

// ── playback ─────────────────────────────────────────────────────────────────

const audio = typeof Audio === "undefined" ? null : new Audio();
let token = 0;

async function play(c: CallEntry): Promise<void> {
  if (!audio) return;
  const mine = ++token;
  const was = state.playing;
  set({ playing: c, position: 0, duration: c.record.call_length_ms / 1000, played: was && was !== c ? [was, ...state.played.filter((x) => x !== was)].slice(0, PLAYED_MAX) : state.played });
  let url: string;
  try {
    url = await transport.callUrl(c.path, "m4a");
  } catch {
    if (mine === token) next();
    return;
  }
  if (mine !== token) return;
  audio.src = url;
  if (state.paused) return;
  audio.play().catch((e: unknown) => {
    // The browser wants a click first: wait for one (Resume).
    if (mine === token && e instanceof DOMException && e.name === "NotAllowedError") set({ paused: true });
  });
}

/** The current call is done: the next queued one, if the feed is on. */
function next(): void {
  const was = state.playing;
  const played = was ? [was, ...state.played.filter((x) => x !== was)].slice(0, PLAYED_MAX) : state.played;
  if (state.feed && !state.paused && state.queue.length) {
    const [c, ...queue] = state.queue;
    set({ queue, played, playing: null });
    void play(c);
  } else {
    token++;
    set({ playing: null, played, position: 0, duration: 0 });
  }
}

if (audio) {
  audio.onended = next;
  audio.onerror = () => {
    if (state.playing) next();
  };
  audio.ontimeupdate = () => set({ position: audio.currentTime });
  audio.ondurationchange = () => Number.isFinite(audio.duration) && set({ duration: audio.duration });
}

export function setFeed(on: boolean): void {
  if (on === state.feed) return;
  if (!on) {
    token++;
    audio?.pause();
    const was = state.playing;
    set({ feed: false, queue: [], missed: 0, playing: null, paused: false, played: was ? [was, ...state.played].slice(0, PLAYED_MAX) : state.played });
    return;
  }
  // One sound at a time: the Live page's live audio stops.
  if (snapshot().listen) setListen(false);
  // (A click: the browser now lets audio play.)
  audio?.play().catch(() => {});
  audio?.pause();
  set({ feed: true, paused: false, missed: 0 });
}

export function togglePause(): void {
  if (!audio) return;
  if (state.paused) {
    set({ paused: false });
    if (state.playing) audio.play().catch(() => set({ paused: true }));
    else if (state.feed && state.queue.length) next();
  } else {
    set({ paused: true });
    audio.pause();
  }
}

export function skip(): void {
  audio?.pause();
  next();
}

/** Play the current call again from the start, else the last one played. */
export function replay(): void {
  if (!audio) return;
  if (state.playing) {
    audio.currentTime = 0;
    if (state.paused) togglePause();
    return;
  }
  const c = state.played[0];
  if (c) {
    set({ paused: false, played: state.played.slice(1) });
    void play(c);
  }
}

/** Play this call now (from the list); the feed goes on after it. */
export function playNow(c: CallEntry): void {
  set({ paused: false, queue: state.queue.filter((x) => x !== c) });
  void play(c);
}

export function seek(s: number): void {
  if (audio && Number.isFinite(s)) audio.currentTime = s;
}

// ── calls from the recorder ──────────────────────────────────────────────────

function arrived(c: CallEntry): void {
  if (state.calls.some((x) => x.path === c.path)) return;
  // (In start order: a long call ends after shorter ones that began later.)
  set({ calls: merge(state.calls, [c]) });
  if (!state.feed || !wanted(state.sel, c)) return;
  let queue = [...state.queue, c];
  let missed = state.missed;
  if (queue.length > QUEUE_MAX) {
    missed += queue.length - QUEUE_MAX;
    queue = queue.slice(-QUEUE_MAX);
  }
  set({ queue, missed });
  if (!state.playing && !state.paused) next();
}

/** Calls (newest first) merged in by start time, without doubles. */
function merge(calls: CallEntry[], more: CallEntry[]): CallEntry[] {
  const seen = new Set(calls.map((c) => c.path));
  const add = more.filter((c) => !seen.has(c.path));
  if (!add.length) return calls;
  return [...calls, ...add].sort((a, b) => b.record.start_time_ms - a.record.start_time_ms).slice(0, CALLS_MAX);
}

/** Ask for calls older than the oldest known (the last 24 hours). */
export function loadOlder(): void {
  if (state.loadingOlder) return;
  const oldest = state.calls.at(-1)?.record.start_time_ms ?? Date.now();
  set({ loadingOlder: true });
  transport.send({ type: "olderCalls", before: oldest, limit: OLDER_PAGE });
}

onRecorderMessage((m) => {
  switch (m.type) {
    case "hello":
      set({ calls: merge(state.calls, m.history) });
      break;
    case "concluded":
      arrived(m.entry);
      break;
    case "callFiles": {
      const fix = (c: CallEntry) => (c.path === m.path ? { ...c, audio: m.audio, json: m.json } : c);
      set({ calls: state.calls.map(fix), queue: state.queue.map(fix).filter(playable) });
      break;
    }
    case "olderCalls":
      set({ calls: merge(state.calls, m.entries), loadingOlder: false, moreOlder: m.more });
      break;
  }
});

// Live audio switched on on the Live page: the feed stops (one sound at a time).
subscribe(() => {
  if (snapshot().listen && state.feed) setFeed(false);
});
