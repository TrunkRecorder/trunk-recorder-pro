// How the interface reaches the recorder. The desktop app: a WebSocket to the
// local trunk-pro server (reconnecting). The web build: the recorder in a Web
// Worker (web/src/web/workerTransport.ts).

import { decodeAudioFrame, type AudioChunk, type FromRecorder, type ToRecorder } from "./protocol.ts";

export type CallExt = "wav" | "m4a" | "json";

export interface Transport {
  readonly kind: "desktop" | "web";
  send(msg: ToRecorder): void;
  onMessage: (msg: FromRecorder) => void;
  onAudio: (chunk: AudioChunk) => void;
  onConnection: (connected: boolean) => void;
  /** Stop for good (the recorder quit): no more reconnecting. */
  close?(): void;
  /** A URL for a recorded file (desktop: the server's /calls/; web: a blob from OPFS). An "m4a" not kept is its WAV. */
  callUrl(path: string, ext: CallExt): Promise<string>;
}

/** A tab hidden this long that then gets a message was likely frozen: what's queued for it is old. */
const FROZEN_MS = 30_000;
/** Whether the tab is hidden now (never, where there's no document: tests). */
const hidden = (): boolean => typeof document !== "undefined" && document.hidden;

export class WsTransport implements Transport {
  readonly kind = "desktop" as const;
  onMessage: (msg: FromRecorder) => void = () => {};
  onAudio: (chunk: AudioChunk) => void = () => {};
  onConnection: (connected: boolean) => void = () => {};
  private ws: WebSocket | null = null;
  private queue: ToRecorder[] = [];
  private retryMs = 500;
  private closed = false;
  /** When the last message was taken, and whether the tab was hidden since. */
  private lastMessage = 0;
  private hiddenSince = false;

  constructor(private readonly url = `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/api/ws`) {
    globalThis.document?.addEventListener("visibilitychange", () => {
      if (hidden()) this.hiddenSince = true;
    });
    this.connect();
  }

  /** A tab frozen in the background is handed every message it missed at once
   *  when it wakes: they'd be shown as if live. Reconnect instead, and be told
   *  how things are now. */
  private stale(): boolean {
    const now = performance.now();
    const frozen = this.hiddenSince && this.lastMessage > 0 && now - this.lastMessage > FROZEN_MS;
    this.lastMessage = now;
    this.hiddenSince = hidden();
    return frozen;
  }

  private connect(): void {
    const ws = new WebSocket(this.url);
    ws.binaryType = "arraybuffer";
    ws.onopen = () => {
      this.retryMs = 500;
      this.lastMessage = 0;
      this.onConnection(true);
      for (const m of this.queue.splice(0)) ws.send(JSON.stringify(m));
    };
    ws.onmessage = (ev) => {
      if (this.stale()) {
        ws.onmessage = null;
        ws.close();
        return;
      }
      if (typeof ev.data === "string") this.onMessage(JSON.parse(ev.data) as FromRecorder);
      else {
        const a = decodeAudioFrame(ev.data as ArrayBuffer);
        if (a) this.onAudio(a);
      }
    };
    ws.onclose = () => {
      this.ws = null;
      if (this.closed) return;
      this.onConnection(false);
      setTimeout(() => this.connect(), this.retryMs);
      this.retryMs = Math.min(5000, this.retryMs * 2);
    };
    this.ws = ws;
  }

  close(): void {
    this.closed = true;
    this.ws?.close();
  }

  send(msg: ToRecorder): void {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(msg));
    else this.queue.push(msg);
  }

  async callUrl(path: string, ext: CallExt): Promise<string> {
    return `/calls/${path.split("/").map(encodeURIComponent).join("/")}.${ext}`;
  }
}
