// The web build keeps calls in the browser's private storage; this shows how
// much, and copies them out to a real folder or a zip (Trunk Recorder's layout).

import { useEffect, useState } from "react";
import { forgetHistory, setNotice, useApp } from "../controller.ts";
import { clearCalls, exportCalls, storageEstimate, zipCalls } from "./opfs.ts";

const mb = (b: number) => (b >= 1e9 ? `${(b / 1e9).toFixed(1)} GB` : b >= 1e7 ? `${(b / 1e6).toFixed(0)} MB` : `${(b / 1e6).toFixed(1)} MB`);

type DirPicker = (o?: { mode?: "readwrite" }) => Promise<FileSystemDirectoryHandle>;

export function BrowserStorage() {
  const s = useApp();
  const [est, setEst] = useState<{ usage: number; quota: number; persisted: boolean } | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  useEffect(() => {
    void storageEstimate().then(setEst, () => {});
  }, [s.history.length]);

  const picker = (window as Window & { showDirectoryPicker?: DirPicker }).showDirectoryPicker;

  async function exportAll() {
    if (!picker) return;
    let dest: FileSystemDirectoryHandle;
    try {
      dest = await picker({ mode: "readwrite" });
    } catch {
      return; // cancelled
    }
    try {
      const n = await exportCalls(dest, (d, t) => setBusy(`Exporting ${d}/${t}…`));
      setNotice(`Exported ${n} calls to “${dest.name}”.`);
    } catch (e) {
      setNotice(`Export failed: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(null);
    }
  }

  async function downloadZip() {
    try {
      const { zip, count } = await zipCalls((d, t) => setBusy(`Zipping ${d}/${t}…`));
      const url = URL.createObjectURL(zip);
      const a = document.createElement("a");
      a.href = url;
      a.download = `calls-${new Date().toISOString().slice(0, 10)}.zip`;
      a.click();
      setTimeout(() => URL.revokeObjectURL(url), 60_000);
      setNotice(`Zipped ${count} calls.`);
    } catch (e) {
      setNotice(`Zip failed: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(null);
    }
  }

  async function clearAll() {
    if (!confirm("Delete every recorded call stored in this browser?")) return;
    await clearCalls();
    forgetHistory();
    setEst(await storageEstimate());
  }

  async function persist() {
    const ok = await navigator.storage.persist?.();
    setEst(await storageEstimate());
    if (!ok) setNotice("Browser declined; calls may be evicted when space is low.");
  }

  return (
    <span className="storage">
      Stored in this browser{est ? ` · ${mb(est.usage)} of ${mb(est.quota)}` : ""}
      {est && !est.persisted && (
        <button className="btn ghost small" onClick={() => void persist()} title="Protect recordings from eviction when space is low">
          Keep
        </button>
      )}
      {busy ? (
        <span className="muted"> {busy}</span>
      ) : (
        <>
          {picker ? (
            <button className="btn ghost small" onClick={() => void exportAll()} disabled={!s.history.length}>
              Export to folder…
            </button>
          ) : null}
          <button className="btn ghost small" onClick={() => void downloadZip()} disabled={!s.history.length}>
            Download zip
          </button>
          <button className="btn ghost small" onClick={() => void clearAll()} disabled={!s.history.length || s.phase !== "idle"}>
            Delete all
          </button>
        </>
      )}
    </span>
  );
}
