// Setup → Recording → Key containers (desktop app): the KFDtool key containers (.ekc) the systems
// name in `keys` that have no `keysPasswordFile`, so wait for a password typed here. The password
// goes to the recorder once, from the recorder's own computer only; it is not stored, not shown
// again, and never sent back. The keys are never sent to any browser: only which are held, by
// algorithm, key id and name.

import { useState } from "react";
import { askKeys, lockKeys, unlockKeys, useApp } from "./controller.ts";
import type { KeyContainer } from "./protocol.ts";

/** A P25 algorithm id by name. */
function cipherName(algId: number): string {
  const names: Record<number, string> = { 0x81: "DES-OFB", 0x83: "3DES", 0x84: "AES-256", 0x85: "AES-128", 0x89: "AES-128", 0xaa: "ADP" };
  return names[algId] ?? `0x${algId.toString(16)}`;
}

export function KeysPanel() {
  const { keys, heldKeys, heldKeysRestricted, canUnlockKeys } = useApp();
  const containers = keys?.containers ?? [];
  const [result, setResult] = useState<string | null>(null);
  const done = (r: { ok: boolean; message: string | null }) => setResult(r.ok ? null : (r.message ?? "Failed."));
  return (
    <section className="panel" aria-labelledby="keys-title">
      <header className="panel-head">
        <h2 id="keys-title">Key containers</h2>
      </header>
      <div className="stack">
        <p className="muted small">
          A system's <code>keys</code> can be a KFDtool key container (<code>.ekc</code>). With <code>keysPasswordFile</code> the recorder opens it by itself; without one it waits here for
          the password. The password stays in memory until you lock the container or quit.{" "}
          <a href="https://github.com/TrunkRecorder/trunk-recorder-pro/blob/main/docs/configuration/systems.md#keys-in-a-kfdtool-container" target="_blank" rel="noreferrer">
            Details
          </a>
        </p>
        {containers.length === 0 && <p className="muted">No system names a key container that needs a password.</p>}
        {containers.map((c) => (
          <Container key={c.file} c={c} can={canUnlockKeys} onResult={done} />
        ))}
        {containers.length > 0 && !canUnlockKeys && (
          <p className="muted small">
            Containers can only be unlocked from the computer the recorder runs on: open this page at <code>http://localhost</code> there (over SSH, forward the port), use a{" "}
            <code>keysPasswordFile</code>, or set <code>server.allowLanUnlock</code> in the config file.
          </p>
        )}
        {result && (
          <p className="bad-text" role="alert">
            {result}
          </p>
        )}
        {containers.some((c) => c.state === "unlocked") &&
          (heldKeysRestricted ? (
            <p className="muted small">Which keys are held is shown on the recorder's own computer only.</p>
          ) : (
            <table className="table">
              <thead>
                <tr>
                  <th>Container</th>
                  <th>Name</th>
                  <th>Cipher</th>
                  <th>Key ID</th>
                </tr>
              </thead>
              <tbody>
                {heldKeys.map((h) => (
                  <tr key={`${h.file}-${h.algId}-${h.keyId}`}>
                    <td className="mono">{h.file}</td>
                    <td>{h.name || "—"}</td>
                    <td>{cipherName(h.algId)}</td>
                    <td className="mono">{h.keyId}</td>
                  </tr>
                ))}
                {heldKeys.length === 0 && (
                  <tr>
                    <td colSpan={4} className="muted">
                      <button className="linkish" onClick={askKeys}>
                        Show the keys held
                      </button>
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          ))}
      </div>
    </section>
  );
}

function Container({ c, can, onResult }: { c: KeyContainer; can: boolean; onResult: (r: { ok: boolean; message: string | null }) => void }) {
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const act = async (f: () => Promise<{ ok: boolean; message: string | null }>) => {
    setBusy(true);
    onResult({ ok: true, message: null });
    const r = await f();
    setBusy(false);
    setPassword("");
    onResult(r);
  };
  const open = c.state === "unlocked";
  return (
    <form
      className="row wrap"
      style={{ gap: 8, alignItems: "flex-end" }}
      onSubmit={(e) => {
        e.preventDefault();
        if (!open && password && !busy) void act(() => unlockKeys(c.file, password));
      }}
    >
      <div className="field" style={{ minWidth: "14em" }}>
        <span className="field-label">{c.file}</span>
        <span className="field-hint">
          Used by {c.systems.join(", ")} · {open ? `unlocked, ${c.count} key${c.count === 1 ? "" : "s"}` : "locked"}
        </span>
      </div>
      {!open && (
        <div className="field">
          <label className="field-label" htmlFor={`keys-pw-${c.file}`}>
            Password
          </label>
          <input
            id={`keys-pw-${c.file}`}
            type="password"
            autoComplete="off"
            name="keys-container-password"
            value={password}
            disabled={!can || busy}
            onChange={(e) => setPassword(e.target.value)}
          />
        </div>
      )}
      {!open && (
        <button className="btn" type="submit" disabled={!can || busy || !password}>
          Unlock
        </button>
      )}
      {open && (
        <button className="btn" type="button" disabled={!can || busy} onClick={() => void act(() => lockKeys(c.file))}>
          Lock
        </button>
      )}
    </form>
  );
}
