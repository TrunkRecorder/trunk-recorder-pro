// client.js — a small client for Trunk Recorder Pro's API (README.md beside
// it). No dependencies: works in browsers and in Node 22+. A recorder serves
// it at /api/client.js, so an interface it serves can just
//
//   import { TrunkClient, LivePlayer } from "/api/client.js";
//
// It keeps the connection up (reconnecting), keeps the recorder's state
// (config, phase, status, calls on the air, recorded calls), re-sends what a
// connection asked for after reconnecting, and plays live audio. The message
// shapes are FromRecorder / ToRecorder in protocol.ts (/api/protocol.ts).

/** The `system` of conventional system 0; conventional system k is 65535 - k. */
export const CONVENTIONAL = 65535;
/** Which conventional system a `system` is, or null for a trunked one. */
export const conventionalIndex = (system) => (system > CONVENTIONAL - 256 && system <= CONVENTIONAL ? CONVENTIONAL - system : null);

/**
 * A live-audio frame (binary message) → { system, callId, talkgroup, samples }
 * (Float32Array, 8000 per second), or null if it isn't one.
 */
export function decodeAudioFrame(buf) {
  const v = new DataView(buf);
  if (buf.byteLength < 11 || v.getUint8(0) !== 2) return null;
  const samples = new Float32Array((buf.byteLength - 11) >> 1);
  for (let i = 0; i < samples.length; i++) samples[i] = v.getInt16(11 + 2 * i, true) / 32768;
  return { system: v.getUint16(1, true), callId: v.getUint32(3, true), talkgroup: v.getUint32(7, true), samples };
}

/**
 * The connection to a recorder.
 *
 *   const rec = new TrunkClient();                       // the page's own recorder
 *   const rec = new TrunkClient({ server: "pi.local:8080" });
 *   rec.on("change", () => draw(rec));                   // anything changed
 *   rec.on("concluded", (m) => console.log(m.entry));    // one message type
 *
 * State (read it, don't change it): connected, version, config, phase
 * ({ phase, error, ended }), status (EngineStatus), sources, calls (on the
 * air now), history (recorded calls, newest first), log (after subscribe(["log"])), unitCsv, aliases,
 * error, exited (the recorder quit; it keeps trying to reconnect).
 */
export class TrunkClient {
  /**
   * @param {object} [opts]
   * @param {string} [opts.server] host:port; default the page's own host (Node: localhost:8080)
   * @param {boolean} [opts.secure] wss:// (default: when the page is https)
   * @param {number} [opts.historyLimit] recorded calls kept in `history` (default 500)
   */
  constructor(opts = {}) {
    const page = typeof location !== "undefined" && location.protocol.startsWith("http") ? location : null;
    this.server = opts.server ?? (page ? page.host : "localhost:8080");
    const secure = opts.secure ?? page?.protocol === "https:";
    this.httpBase = `${secure ? "https" : "http"}://${this.server}`;
    this.wsUrl = `${secure ? "wss" : "ws"}://${this.server}/api/ws`;
    this.historyLimit = opts.historyLimit ?? 500;

    this.connected = false;
    this.version = null;
    this.config = null;
    this.phase = { phase: "idle", error: null, ended: false };
    this.status = null;
    this.sources = [];
    this.calls = [];
    this.history = [];
    this.log = [];
    /** Each system's unit names as saved: short name → CSV (`unit,name` lines; see unitName). */
    this.unitCsv = {};
    /** Talker aliases heard since connecting: short name → { unit: alias }. */
    this.aliases = {};
    this._unitCache = new Map();
    /** The latest `error` message from the recorder (a command that failed). */
    this.error = null;
    /** The recorder said it was exiting (`quit`); cleared when it's back. */
    this.exited = false;

    this._handlers = new Map();
    this._listen = null;
    this._topics = [];
    this._ws = null;
    this._queue = [];
    this._retryMs = 500;
    this._closed = false;
    this._connect();
  }

  /**
   * Call `fn` for: a message type ("status", "concluded", … — the message),
   * "message" (every message), "audio" (a decoded live-audio chunk),
   * "connection" (true / false), "change" (state changed: redraw).
   * Returns a function that stops it.
   */
  on(type, fn) {
    if (!this._handlers.has(type)) this._handlers.set(type, new Set());
    this._handlers.get(type).add(fn);
    return () => this._handlers.get(type)?.delete(fn);
  }

  /** Send a command (ToRecorder in protocol.ts); queued while disconnected. */
  send(msg) {
    if (this._ws?.readyState === 1) this._ws.send(JSON.stringify(msg));
    else this._queue.push(msg);
  }

  start() {
    this.send({ type: "start" });
  }

  stop() {
    this.send({ type: "stop" });
  }

  /**
   * Live audio on this connection: listen() every call, listen({ talkgroup })
   * one talkgroup's, listen({ system, talkgroup }) one system's (`system`: its
   * short name), listen(false) none.
   * Kept across reconnects. Chunks come to on("audio", …).
   */
  /**
   * Costly messages come only when asked for, while you show them:
   * subscribe(["log", "spectrum:0"]) — the control channel log, a radio's
   * waterfall (also "rf:<source>", "decode:<shortName>", "platform").
   * Replaces the last; kept across reconnects. subscribe([]) stops them.
   */
  subscribe(topics) {
    this._topics = [...topics];
    this.send({ type: "subscribe", topics: this._topics });
  }

  listen(filter = {}) {
    this._listen = filter === false ? null : { system: filter.system ?? null, talkgroup: filter.talkgroup ?? null };
    this.send({ type: "listen", on: !!this._listen, system: this._listen?.system ?? null, talkgroup: this._listen?.talkgroup ?? null });
  }

  /**
   * Change the config: `edit` gets a copy of the latest one to change; the
   * whole config is sent back. Everyone gets the new one as a `config`
   * message. Recording uses it from the next start.
   *
   *   rec.setConfig((c) => { c.recording.minCallS = 2; });
   */
  setConfig(edit) {
    if (!this.config) throw new Error("No config yet: wait for the first change event.");
    const next = structuredClone(this.config);
    edit(next);
    this.send({ type: "setConfig", config: next });
  }

  /**
   * A radio's name on system `shortName`: from its unit names file (a unit
   * between slashes there is a regular expression: `/^1(\d{3})$/,Engine $1`),
   * else the talker alias heard, else "".
   */
  unitName(shortName, unit) {
    if (!this._unitCache.has(shortName)) {
      const rows = [];
      for (const line of (this.unitCsv[shortName] ?? "").split(/\r?\n/)) {
        const at = line.indexOf(",");
        if (at < 0) continue;
        const [key, name] = [line.slice(0, at).trim(), line.slice(at + 1).trim().replace(/^"|"$/g, "")];
        const re = /^\/(.*)\/$/.exec(key);
        try {
          rows.push(re ? { re: new RegExp(re[1]), name } : { unit: Number(key), name });
        } catch {}
      }
      this._unitCache.set(shortName, rows);
    }
    const id = String(unit);
    for (const r of this._unitCache.get(shortName)) {
      if (r.re ? r.re.test(id) : r.unit === unit) return r.re ? id.replace(r.re, r.name) : r.name;
    }
    return this.aliases[shortName]?.[unit] ?? "";
  }

  /**
   * System `shortName`'s talkgroup file (Trunk Recorder's CSV, as in the
   * config): Map talkgroup → { talkgroup, alphaTag, description, tag, group, mode }.
   */
  talkgroups(shortName) {
    const csv = this.config?.systems.find((s) => s.shortName === shortName)?.talkgroupsCsv ?? "";
    this._tgCache ??= new Map();
    const hit = this._tgCache.get(shortName);
    if (hit?.csv === csv) return hit.map;
    const map = parseTalkgroups(csv);
    this._tgCache.set(shortName, { csv, map });
    return map;
  }

  /** A recorded call's file: entry (or its path), "wav" | "json" | "m4a". */
  callUrl(entry, ext = "wav") {
    const path = typeof entry === "string" ? entry : entry.path;
    return `${this.httpBase}/calls/${path.split("/").map(encodeURIComponent).join("/")}.${ext}`;
  }

  /** Stop for good. */
  close() {
    this._closed = true;
    this._ws?.close();
  }

  // ── internals ──────────────────────────────────────────────────────────────

  _emit(type, value) {
    for (const fn of this._handlers.get(type) ?? []) {
      try {
        fn(value);
      } catch (e) {
        console.error(e);
      }
    }
  }

  _connect() {
    const ws = new WebSocket(this.wsUrl);
    ws.binaryType = "arraybuffer";
    ws.onopen = () => {
      this._retryMs = 500;
      for (const m of this._queue.splice(0)) ws.send(JSON.stringify(m));
    };
    ws.onmessage = (ev) => {
      if (typeof ev.data !== "string") {
        const chunk = decodeAudioFrame(ev.data);
        if (chunk) this._emit("audio", chunk);
        return;
      }
      const m = JSON.parse(ev.data);
      this._apply(m);
      this._emit(m.type, m);
      this._emit("message", m);
      this._emit("change", this);
    };
    ws.onclose = () => {
      this._ws = null;
      if (this.connected) {
        this.connected = false;
        this._emit("connection", false);
        this._emit("change", this);
      }
      if (this._closed) return;
      setTimeout(() => this._connect(), this._retryMs);
      this._retryMs = Math.min(5000, this._retryMs * 2);
    };
    this._ws = ws;
  }

  _apply(m) {
    switch (m.type) {
      case "hello":
        Object.assign(this, { version: m.version, config: m.config, phase: m.phase, history: m.history, error: null, exited: false });
        this.unitCsv = m.units ?? {};
        this._unitCache.clear();
        if (!this.connected) {
          this.connected = true;
          this._emit("connection", true);
        }
        // Listening is per connection: ask again.
        if (this._listen) this.send({ type: "listen", on: true, ...this._listen });
        if (this._topics.length) this.send({ type: "subscribe", topics: this._topics });
        break;
      case "state":
        this.phase = { phase: m.phase, error: m.error, ended: m.ended };
        if (m.phase !== "running") this.calls = [];
        break;
      case "status":
        Object.assign(this, { status: m.status, sources: m.sources, calls: m.calls });
        break;
      case "config":
        this.config = m.config;
        break;
      case "concluded":
        this.history = [m.entry, ...this.history].slice(0, this.historyLimit);
        break;
      case "log":
        this.log = [...this.log, ...m.lines].slice(-500);
        break;
      case "unitAlias":
        this.aliases[m.system] = { ...this.aliases[m.system], [m.unit]: m.alias };
        break;
      case "error":
        this.error = m.message;
        break;
      case "quit":
        // The recorder is exiting. It may well be restarted (a service, an
        // update): keep trying, every 5 s.
        this.exited = true;
        this._retryMs = 5000;
        break;
    }
  }
}

/** Trunk Recorder's talkgroup CSV (with a "Decimal,…" header, or the old fixed columns) → Map. */
export function parseTalkgroups(csv) {
  const split = (line) => {
    const out = [];
    let cur = "",
      quoted = false;
    for (let i = 0; i < line.length; i++) {
      const ch = line[i];
      if (quoted && ch === '"' && line[i + 1] === '"') (cur += '"'), i++;
      else if (ch === '"') quoted = !quoted;
      else if (ch === "," && !quoted) out.push(cur.trim()), (cur = "");
      else cur += ch;
    }
    out.push(cur.trim());
    return out;
  };
  const lines = csv.split(/\r?\n/).filter((l) => l.trim() && !l.trim().startsWith("#"));
  const map = new Map();
  if (!lines.length) return map;
  const head = split(lines[0]);
  const headed = head[0] === "Decimal";
  // Without a header: Decimal, Hex, Mode, Alpha Tag, Description, Tag, Category.
  const at = (f, name, legacy) => (headed ? f[head.indexOf(name)] : f[legacy]) ?? "";
  for (const line of headed ? lines.slice(1) : lines) {
    const f = split(line);
    const talkgroup = parseInt(f[0], 10);
    if (!Number.isFinite(talkgroup)) continue;
    map.set(talkgroup, { talkgroup, alphaTag: at(f, "Alpha Tag", 3), description: at(f, "Description", 4), tag: at(f, "Tag", 5), group: at(f, "Category", 6), mode: at(f, "Mode", 2) });
  }
  return map;
}

/**
 * Plays live audio: give it the chunks from on("audio", …).
 *
 *   const player = new LivePlayer();
 *   button.onclick = () => { player.resume(); rec.listen(); };  // browsers need a click first
 *   rec.on("audio", (chunk) => player.play(chunk));
 *
 * With `followCall` (the default) it plays one call at a time: the first
 * one heard, until it has been quiet for `quietS`, then the next — calls
 * overlapping it are skipped. Without, every chunk is queued in turn.
 */
export class LivePlayer {
  /**
   * @param {object} [opts]
   * @param {boolean} [opts.followCall] one call at a time (default true)
   * @param {number} [opts.quietS] a call is over after this long without audio, s (default 1.5)
   * @param {number} [opts.gain] volume (default 2: the vocoders' output is quiet)
   * @param {(call: {callId:number, system:number, talkgroup:number} | null) => void} [opts.onNowPlaying]
   */
  constructor(opts = {}) {
    this.followCall = opts.followCall ?? true;
    this.quietS = opts.quietS ?? 1.5;
    this.gainValue = opts.gain ?? 2;
    this.onNowPlaying = opts.onNowPlaying ?? (() => {});
    /** The call playing: { callId, system, talkgroup }, or null. */
    this.nowPlaying = null;
    this._ctx = null;
    this._next = 0;
    this._lastAt = 0;
    this._timer = null;
  }

  /** Make or wake the audio output: call it from a click or key press. */
  resume() {
    if (!this._ctx) {
      this._ctx = new AudioContext();
      this._gain = this._ctx.createGain();
      this._gain.gain.value = this.gainValue;
      this._gain.connect(this._ctx.destination);
    }
    return this._ctx.resume();
  }

  /** Queue a chunk from on("audio", …). */
  play(chunk) {
    const ctx = this._ctx;
    if (!ctx || ctx.state !== "running" || !chunk.samples.length) return;
    const now = ctx.currentTime;
    if (this.followCall) {
      const over = this.nowPlaying && now - this._lastAt > this.quietS && this._next <= now;
      if (!this.nowPlaying || over) this._setPlaying(chunk);
      else if (chunk.callId !== this.nowPlaying.callId) return;
    } else if (this.nowPlaying?.callId !== chunk.callId) this._setPlaying(chunk);
    this._lastAt = now;
    const buf = ctx.createBuffer(1, chunk.samples.length, 8000);
    buf.getChannelData(0).set(chunk.samples);
    const src = ctx.createBufferSource();
    src.buffer = buf;
    src.connect(this._gain);
    // Audio comes in bursts: queue after the last chunk; after a gap, start a little ahead.
    if (this._next < now) this._next = now + 0.25;
    src.start(this._next);
    this._next += buf.duration;
    clearTimeout(this._timer);
    this._timer = setTimeout(() => this._setPlaying(null), (this._next - now + this.quietS) * 1000);
  }

  /** Stop and release the audio output. */
  stop() {
    clearTimeout(this._timer);
    this._ctx?.close();
    this._ctx = null;
    this._setPlaying(null);
  }

  _setPlaying(chunk) {
    const next = chunk ? { callId: chunk.callId, system: chunk.system, talkgroup: chunk.talkgroup } : null;
    if (next?.callId === this.nowPlaying?.callId) return;
    this.nowPlaying = next;
    this.onNowPlaying(next);
  }
}

/** "1:05" for 65 s. */
export const duration = (s) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;

/** "155.2000 MHz" for 155200000. */
export const mhz = (hz) => (hz ? `${(hz / 1e6).toFixed(4)} MHz` : "—");

/** Text safe to put in HTML. */
export const escapeHtml = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
