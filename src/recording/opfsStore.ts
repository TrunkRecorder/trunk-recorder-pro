// Call storage in the Origin Private File System — the browser's private disk.
//
//   calls/<shortName>/<YYYY-MM-DD>/<base>.wav + <base>.json   (Trunk Recorder's layout)
//   calls/index.ndjson                                          one CallEntry per line
//
// Writes happen in the trunk worker through synchronous access handles (worker
// only, and the fastest OPFS path); the page reads the index and the files.

import type { CallRecordJson } from "./callRecord.ts";

export interface StoredCall {
  baseName: string;
  dir: string;
  record: CallRecordJson;
}

const INDEX = "index.ndjson";

async function callsRoot(): Promise<FileSystemDirectoryHandle> {
  const root = await navigator.storage.getDirectory();
  return root.getDirectoryHandle("calls", { create: true });
}

async function dirAt(path: string, create: boolean): Promise<FileSystemDirectoryHandle> {
  let d = await callsRoot();
  for (const part of path.split("/").filter(Boolean)) d = await d.getDirectoryHandle(part, { create });
  return d;
}

/** Worker only: write bytes to a file (replacing it). */
async function writeFile(dir: FileSystemDirectoryHandle, name: string, bytes: Uint8Array, append = false): Promise<void> {
  const fh = await dir.getFileHandle(name, { create: true });
  const h = await (fh as FileSystemFileHandle & { createSyncAccessHandle(): Promise<SyncHandle> }).createSyncAccessHandle();
  try {
    const at = append ? h.getSize() : 0;
    if (!append) h.truncate(0);
    h.write(bytes, { at });
    h.flush();
  } finally {
    h.close();
  }
}

interface SyncHandle {
  getSize(): number;
  truncate(n: number): void;
  write(b: Uint8Array, o: { at: number }): number;
  flush(): void;
  close(): void;
}

function dayOf(epochS: number): string {
  const d = new Date(epochS * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/** Worker only. */
export async function saveCall(shortName: string, baseName: string, record: CallRecordJson, wav: Uint8Array): Promise<StoredCall> {
  const dir = `${shortName}/${dayOf(record.start_time)}`;
  const d = await dirAt(dir, true);
  const enc = new TextEncoder();
  await writeFile(d, `${baseName}.wav`, wav);
  await writeFile(d, `${baseName}.json`, enc.encode(JSON.stringify(record, null, 2)));
  const entry: StoredCall = { baseName, dir, record };
  await writeFile(await callsRoot(), INDEX, enc.encode(JSON.stringify(entry) + "\n"), true);
  return entry;
}

/** Most recent calls first. */
export async function listCalls(limit = 500): Promise<StoredCall[]> {
  try {
    const fh = await (await callsRoot()).getFileHandle(INDEX);
    const text = await (await fh.getFile()).text();
    const lines = text.split("\n").filter(Boolean);
    const out: StoredCall[] = [];
    for (let i = lines.length - 1; i >= 0 && out.length < limit; i--) {
      try {
        out.push(JSON.parse(lines[i]) as StoredCall);
      } catch {
        /* a torn last line after a crash */
      }
    }
    return out;
  } catch {
    return [];
  }
}

export async function callFile(c: StoredCall, ext: "wav" | "json"): Promise<File> {
  const d = await dirAt(c.dir, false);
  return (await d.getFileHandle(`${c.baseName}.${ext}`)).getFile();
}

export async function clearCalls(): Promise<void> {
  const root = await navigator.storage.getDirectory();
  await root.removeEntry("calls", { recursive: true }).catch(() => {});
}

export async function storageEstimate(): Promise<{ usage: number; quota: number; persisted: boolean }> {
  const e = await navigator.storage.estimate();
  const persisted = (await navigator.storage.persisted?.()) ?? false;
  return { usage: e.usage ?? 0, quota: e.quota ?? 0, persisted };
}
