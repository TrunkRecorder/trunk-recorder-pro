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

/** The recorder sends every connection its host status every 2 s (monitor.rs's `EVERY`):
 *  nothing for this long, and the tab wasn't running (frozen in the background, the
 *  computer asleep). What's queued for it then is old. */
const FROZEN_MS = 30_000;

/** Whether a message at `now` comes too late after the last one (`last`, 0: none yet) to be live. ms. */
export const frozen = (last: number, now: number): boolean => last > 0 && now - last > FROZEN_MS;

/** A host status this much later than the most timely one on this connection, and the tab is
 *  taking in a backlog (slowed in the background, never quite stopped). ms. */
const BEHIND_MS = 10_000;

export class WsTransport implements Transport {
  readonly kind = "desktop" as const;
  onMessage: (msg: FromRecorder) => void = () => {};
  onAudio: (chunk: AudioChunk) => void = () => {};
  onConnection: (connected: boolean) => void = () => {};
  private ws: WebSocket | null = null;
  private queue: ToRecorder[] = [];
  private retryMs = 500;
  private closed = false;
  /** When the last message came, ms. */
  private lastMessage = 0;
  /** The least a host status has lagged the server's clock on this connection, ms: clock skew and latency. */
  private leastLag = Infinity;

  constructor(private readonly url = `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/api/ws`) {
    this.connect();
  }

  private connect(): void {
    const ws = new WebSocket(this.url);
    ws.binaryType = "arraybuffer";
    ws.onopen = () => {
      this.retryMs = 500;
      this.lastMessage = 0;
      this.leastLag = Infinity;
      this.onConnection(true);
      for (const m of this.queue.splice(0)) ws.send(JSON.stringify(m));
    };
    ws.onmessage = (ev) => {
      // A tab that wasn't running is handed everything it missed at once when it wakes: it'd be
      // shown as if live. Reconnected instead, it's told how things are now.
      const now = Date.now();
      if (frozen(this.lastMessage, now)) {
        ws.onmessage = null;
        ws.close();
        return;
      }
      this.lastMessage = now;
      if (typeof ev.data === "string") {
        const m = JSON.parse(ev.data) as FromRecorder;
        if (m.type === "host") {
          const lag = now - m.t * 1000;
          this.leastLag = Math.min(this.leastLag, lag);
          if (lag - this.leastLag > BEHIND_MS) {
            ws.onmessage = null;
            ws.close();
            return;
          }
        }
        this.onMessage(m);
      } else {
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
