// How the interface reaches the recorder. The desktop app: a WebSocket to the
// local trunk-pro server (reconnecting). The web build: the recorder in a Web
// Worker (web/src/web/workerTransport.ts).

import { decodeAudioFrame, type AudioChunk, type FromRecorder, type ToRecorder } from "./protocol.ts";

export interface Transport {
  readonly kind: "desktop" | "web";
  send(msg: ToRecorder): void;
  onMessage: (msg: FromRecorder) => void;
  onAudio: (chunk: AudioChunk) => void;
  onConnection: (connected: boolean) => void;
  /** Stop for good (the recorder quit): no more reconnecting. */
  close?(): void;
  /** A URL for a recorded file (desktop: the server's /calls/; web: a blob from OPFS). */
  callUrl(path: string, ext: "wav" | "json"): Promise<string>;
}

export class WsTransport implements Transport {
  readonly kind = "desktop" as const;
  onMessage: (msg: FromRecorder) => void = () => {};
  onAudio: (chunk: AudioChunk) => void = () => {};
  onConnection: (connected: boolean) => void = () => {};
  private ws: WebSocket | null = null;
  private queue: ToRecorder[] = [];
  private retryMs = 500;
  private closed = false;

  constructor(private readonly url = `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/api/ws`) {
    this.connect();
  }

  private connect(): void {
    const ws = new WebSocket(this.url);
    ws.binaryType = "arraybuffer";
    ws.onopen = () => {
      this.retryMs = 500;
      this.onConnection(true);
      for (const m of this.queue.splice(0)) ws.send(JSON.stringify(m));
    };
    ws.onmessage = (ev) => {
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
      // Refused or logged out (not just disconnected): back to the login page.
      fetch("/api/whoami").then((r) => r.status === 401 && location.reload(), () => {});
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

  async callUrl(path: string, ext: "wav" | "json"): Promise<string> {
    return `/calls/${path.split("/").map(encodeURIComponent).join("/")}.${ext}`;
  }
}
