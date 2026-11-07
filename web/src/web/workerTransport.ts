// The web build's transport: the recorder runs in a Web Worker in this page.
// The page keeps the config (localStorage) and the chosen capture files, asks
// for WebUSB access (it needs a click), and reads recorded calls from OPFS.

import { decodeAudioFrame, type AudioChunk, type Config, type FromRecorder, type Talkgroup, type ToRecorder, type UnitName } from "../protocol.ts";
import type { CallExt, Transport } from "../transport.ts";
import { defaultConfig, storedConfig } from "../config.ts";
import { migrateTalkgroups, migrateUnitNames } from "../migrate.ts";
import type { FromWorker, KeptNames, ToWorker } from "./engine.worker.ts";
import { callBlob } from "./opfs.ts";

const CONFIG_KEY = "trp.config";
/** Each system's talkgroups (JSON rows), by short name: kept beside the config, not in it. */
const TALKGROUPS_KEY = "trp.talkgroups";
const UNIT_NAMES_KEY = "trp.unitNames";

function loadConfig(): Config {
  try {
    const raw = localStorage.getItem(CONFIG_KEY);
    if (raw) return storedConfig(JSON.parse(raw) as Partial<Config>);
  } catch {
    /* storage blocked or corrupt: defaults */
  }
  return defaultConfig();
}

/** Each system's names, as this browser stored them (the worker keeps them, and says when they change). */
function loadNames(): KeptNames {
  const read = <T>(key: string): Record<string, T> => {
    try {
      return JSON.parse(localStorage.getItem(key) ?? "{}") as Record<string, T>;
    } catch {
      return {};
    }
  };
  try {
    // (With what an older version kept read in: migrate.ts.)
    const raw = JSON.parse(localStorage.getItem(CONFIG_KEY) ?? "{}");
    return { talkgroups: migrateTalkgroups(raw, read<Talkgroup[]>(TALKGROUPS_KEY)), unitNames: migrateUnitNames(raw, read<UnitName[]>(UNIT_NAMES_KEY)) };
  } catch {
    return { talkgroups: {}, unitNames: read<UnitName[]>(UNIT_NAMES_KEY) };
  }
}

function saveNames(n: KeptNames): void {
  try {
    localStorage.setItem(TALKGROUPS_KEY, JSON.stringify(n.talkgroups));
    localStorage.setItem(UNIT_NAMES_KEY, JSON.stringify(n.unitNames));
  } catch {
    /* not persisted; still used this session */
  }
}

export const RTL_USB_FILTERS = [
  { vendorId: 0x0bda, productId: 0x2838 },
  { vendorId: 0x0bda, productId: 0x2832 },
];

function saveConfig(c: Config): void {
  try {
    localStorage.setItem(CONFIG_KEY, JSON.stringify(c));
  } catch {
    /* not persisted; still used this session */
  }
}

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
      if ("keep" in d) saveNames(d.keep);
      else if ("audio" in d) {
        const a = decodeAudioFrame(d.audio);
        if (a) this.onAudio(a);
      } else {
        // The recorder changed the config itself (a call sign heard on the air): kept.
        if (d.msg.type === "config") saveConfig(d.msg.config);
        this.onMessage(d.msg);
      }
    };
    this.worker.onerror = (e) => this.onMessage({ type: "error", message: `Engine worker crashed: ${e.message}` });
    this.post({ type: "init", config: loadConfig(), names: loadNames() });
    queueMicrotask(() => this.onConnection(true));
  }

  private post(m: ToWorker): void {
    this.worker.postMessage(m);
  }

  send(msg: ToRecorder): void {
    if (msg.type === "setConfig") saveConfig(msg.config);
    if (msg.type === "start" || msg.type === "surveyStart" || msg.type === "profileSource") this.post({ type: "files", files: this.files });
    this.post(msg);
  }

  private urls = new Map<string, string>();

  async callUrl(path: string, want: CallExt): Promise<string> {
    // The browser version keeps WAV only.
    const ext = want === "m4a" ? "wav" : want;
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
