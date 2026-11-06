// Recorded calls in the browser's private storage (OPFS), in Trunk Recorder's
// layout: calls/<system>/<year>/<month>/<day>/<base>.wav|json, plus
// calls/index.ndjson (one history entry per line). Usable from the worker
// (writes) and the page (reads, export, delete).

import type { CallEntry } from "../protocol.ts";
import { ZipWriter } from "./zip.ts";

async function dirFor(path: string[], create: boolean): Promise<FileSystemDirectoryHandle> {
  let d = await navigator.storage.getDirectory();
  for (const p of path) d = await d.getDirectoryHandle(p, { create });
  return d;
}

async function writeFile(dir: FileSystemDirectoryHandle, name: string, data: BlobPart): Promise<void> {
  const f = await dir.getFileHandle(name, { create: true });
  const w = await f.createWritable();
  await w.write(data);
  await w.close();
}

/** Store a concluded call (rel = "<system>/<y>/<m>/<d>/<base>"). */
export async function saveCall(rel: string, wav: Uint8Array, json: string, entry: CallEntry, raw?: Uint8Array): Promise<void> {
  const parts = rel.split("/");
  const base = parts.pop()!;
  const dir = await dirFor(["calls", ...parts], true);
  await writeFile(dir, `${base}.wav`, new Blob([wav as Uint8Array<ArrayBuffer>], { type: "audio/wav" }));
  if (raw) await writeFile(dir, `${base}.raw`, raw as Uint8Array<ArrayBuffer>);
  await writeFile(dir, `${base}.json`, json);
  // Append to the index (read-modify-write: calls are seconds apart).
  const root = await dirFor(["calls"], true);
  const h = await root.getFileHandle("index.ndjson", { create: true });
  const old = await (await h.getFile()).text();
  await writeFile(root, "index.ndjson", old + JSON.stringify(entry) + "\n");
}

/** Store an independent discriminator capture (rel = "<system>/<y>/<m>/<d>/<base>"). */
export async function saveRawCapture(rel: string, raw: Uint8Array): Promise<void> {
  const parts = rel.split("/");
  const base = parts.pop()!;
  const dir = await dirFor(["calls", "raw-captures", ...parts], true);
  await writeFile(dir, `${base}.raw`, raw as Uint8Array<ArrayBuffer>);
}

async function rawCaptures(): Promise<{ path: string; file: File }[]> {
  const found: { path: string; file: File }[] = [];
  const root = await dirFor(["calls", "raw-captures"], false).catch(() => null);
  if (!root) return found;
  const walk = async (dir: FileSystemDirectoryHandle, prefix: string): Promise<void> => {
    const entries = (dir as FileSystemDirectoryHandle & { entries(): AsyncIterableIterator<[string, FileSystemHandle]> }).entries();
    for await (const [name, entry] of entries) {
      const path = prefix ? `${prefix}/${name}` : name;
      if (entry.kind === "directory") await walk(entry as FileSystemDirectoryHandle, path);
      else if (name.endsWith(".raw")) found.push({ path, file: await (entry as FileSystemFileHandle).getFile() });
    }
  };
  await walk(root, "");
  return found;
}

/** Newest first. */
export async function listCalls(limit = 300): Promise<CallEntry[]> {
  try {
    const root = await dirFor(["calls"], false);
    const text = await (await (await root.getFileHandle("index.ndjson")).getFile()).text();
    const out: CallEntry[] = [];
    for (const line of text.split("\n")) {
      if (!line.trim()) continue;
      try {
        out.push(JSON.parse(line) as CallEntry);
      } catch {
        /* a torn line */
      }
    }
    return out.reverse().slice(0, limit);
  } catch {
    return [];
  }
}

export async function callBlob(path: string, ext: "wav" | "raw" | "json"): Promise<Blob> {
  const parts = path.split("/");
  const base = parts.pop()!;
  const dir = await dirFor(["calls", ...parts], false);
  return (await dir.getFileHandle(`${base}.${ext}`)).getFile();
}

export async function readText(name: string): Promise<string | null> {
  try {
    const root = await navigator.storage.getDirectory();
    return await (await (await root.getFileHandle(name)).getFile()).text();
  } catch {
    return null;
  }
}

export async function writeText(name: string, text: string): Promise<void> {
  await writeFile(await navigator.storage.getDirectory(), name, text);
}

export async function storageEstimate(): Promise<{ usage: number; quota: number; persisted: boolean }> {
  const e = await navigator.storage.estimate();
  return { usage: e.usage ?? 0, quota: e.quota ?? 0, persisted: (await navigator.storage.persisted?.()) ?? false };
}

/** Copy every call (and the index) into a folder the user picked. */
export async function exportCalls(dest: FileSystemDirectoryHandle, progress: (done: number, total: number) => void): Promise<number> {
  const entries = await listCalls(1_000_000);
  const captures = await rawCaptures();
  const total = entries.length + captures.length;
  let done = 0;
  for (const e of entries) {
    const parts = e.path.split("/");
    const base = parts.pop()!;
    let d = dest;
    for (const p of parts) d = await d.getDirectoryHandle(p, { create: true });
    for (const ext of ["wav", "raw", "json"] as const) {
      try {
        await writeFile(d, `${base}.${ext}`, await callBlob(e.path, ext));
      } catch {
        /* missing file: skip */
      }
    }
    progress(++done, total);
  }
  for (const capture of captures) {
    const parts = capture.path.split("/");
    const base = parts.pop()!;
    let d = dest;
    for (const p of ["raw-captures", ...parts]) d = await d.getDirectoryHandle(p, { create: true });
    await writeFile(d, base, capture.file);
    progress(++done, total);
  }
  const root = await dirFor(["calls"], false).catch(() => null);
  const index = await (await root?.getFileHandle("index.ndjson").catch(() => null))?.getFile();
  if (index) await writeFile(dest, "index.ndjson", index);
  return done;
}

/** Every call (and the index) as one zip, in the same layout as exportCalls. */
export async function zipCalls(progress: (done: number, total: number) => void): Promise<{ zip: Blob; count: number }> {
  const entries = await listCalls(1_000_000);
  const captures = await rawCaptures();
  const zip = new ZipWriter();
  const total = entries.length + captures.length;
  let done = 0;
  for (const e of entries) {
    for (const ext of ["wav", "raw", "json"] as const) {
      let f: File;
      try {
        f = (await callBlob(e.path, ext)) as File;
      } catch {
        continue; // missing file: skip
      }
      await zip.add(`${e.path}.${ext}`, f, f.lastModified);
    }
    progress(++done, total);
  }
  for (const capture of captures) {
    await zip.add(`raw-captures/${capture.path}`, capture.file, capture.file.lastModified);
    progress(++done, total);
  }
  const root = await dirFor(["calls"], false).catch(() => null);
  if (root) {
    const index = await (await root.getFileHandle("index.ndjson").catch(() => null))?.getFile();
    if (index) await zip.add("index.ndjson", index, index.lastModified);
  }
  return { zip: zip.finish(), count: done };
}

export async function clearCalls(): Promise<void> {
  const root = await navigator.storage.getDirectory();
  await root.removeEntry("calls", { recursive: true }).catch(() => {});
}
