// The web build's transport: the recorder runs in a Web Worker in this page.
// The page keeps the config (localStorage) and the chosen capture files, asks
// for WebUSB access (it needs a click), and reads recorded calls from OPFS.

import { decodeAudioFrame, type AudioChunk, type Config, type FromRecorder, type ToRecorder } from "../protocol.ts";
import type { Transport } from "../transport.ts";
import { defaultConfig, storedConfig } from "../config.ts";
import type { FromWorker, ToWorker } from "./engine.worker.ts";
import { callBlob } from "./opfs.ts";

const CONFIG_KEY = "trp.config";

function loadConfig(): Config {
  try {
    const raw = localStorage.getItem(CONFIG_KEY);
    if (raw) return storedConfig(JSON.parse(raw) as Partial<Config>);
  } catch {
    /* storage blocked or corrupt: defaults */
  }
  return defaultConfig();
}

export const RTL_USB_FILTERS = [
  { vendorId: 0x0bda, productId: 0x2838 },
  { vendorId: 0x0bda, productId: 0x2832 },
];

export class WorkerTransport implements Transport {
  readonly kind = "web" as const;
  onMessage: (msg: FromRecorder) => void = () => {};
  onAudio: (chunk: AudioChunk) => void = () => {};
  onConnection: (connected: boolean) => void = () => {};
  private worker: Worker;
  private files: (File | null)[] = [];

  constructor() {
    this.worker = new Worker(new URL("./engine.worker.ts", import.meta.url), { type: "module" });
    this.worker.onmessage = (ev: MessageEvent<FromWorker>) => {
      const d = ev.data;
      if ("audio" in d) {
        const a = decodeAudioFrame(d.audio);
        if (a) this.onAudio(a);
      } else this.onMessage(d.msg);
    };
    this.worker.onerror = (e) => this.onMessage({ type: "error", message: `Engine worker crashed: ${e.message}` });
    this.post({ type: "init", config: loadConfig() });
    queueMicrotask(() => this.onConnection(true));
  }

  private post(m: ToWorker): void {
    this.worker.postMessage(m);
  }

  send(msg: ToRecorder): void {
    if (msg.type === "setConfig") {
      try {
        localStorage.setItem(CONFIG_KEY, JSON.stringify(msg.config));
      } catch {
        /* not persisted; still used this session */
      }
    }
    // Desktop-only (no plugins in the browser either).
    if (msg.type === "quit" || msg.type === "findRadios" || msg.type === "channelFile" || msg.type === "listDir" || msg.type === "readTrConfig") return;
    if (
      msg.type === "plugins" ||
      msg.type === "addPlugin" ||
      msg.type === "removePlugin" ||
      msg.type === "pluginStore" ||
      msg.type === "installPlugin"
    )
      return;
    if (msg.type === "start" || msg.type === "surveyStart") this.post({ type: "files", files: this.files });
    this.post(msg);
  }

  private urls = new Map<string, string>();

  async callUrl(path: string, ext: "wav" | "json"): Promise<string> {
    const key = `${path}.${ext}`;
    let u = this.urls.get(key);
    if (!u) {
      u = URL.createObjectURL(await callBlob(path, ext));
      this.urls.set(key, u);
      // Keep a handful; blob URLs pin their data in memory.
      if (this.urls.size > 16) {
        const [k, old] = this.urls.entries().next().value!;
        URL.revokeObjectURL(old);
        this.urls.delete(k);
      }
    }
    return u;
  }

  /** A capture file for source `index` (the browser can't reopen a path). */
  setFile(index: number, file: File | null): void {
    this.files[index] = file;
  }
  removeFile(index: number): void {
    this.files.splice(index, 1);
  }
  file(index: number): File | null {
    return this.files[index] ?? null;
  }

  /** Ask for a dongle (must run from a click). */
  async requestUsb(): Promise<boolean> {
    const usb = (navigator as Navigator & { usb?: { requestDevice(o: { filters: typeof RTL_USB_FILTERS }): Promise<unknown> } }).usb;
    if (!usb) throw new Error("This browser has no WebUSB. Use Chrome or Edge (desktop, or Android with USB-OTG).");
    try {
      await usb.requestDevice({ filters: RTL_USB_FILTERS });
    } catch (e) {
      if (e instanceof DOMException && e.name === "NotFoundError") return false; // chooser cancelled
      throw e;
    }
    this.post({ type: "devices" });
    return true;
  }
}
