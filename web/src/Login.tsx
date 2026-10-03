// The login page (desktop app), shown before the interface when the recorder
// wants a login. (With no accounts yet, the recorder's own computer goes
// straight in, and makes the first admin in Setup → Accounts.)

import { useState } from "react";
import type { WhoAmI } from "./protocol.ts";

export function Login(props: { who: WhoAmI }) {
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(props.who.problem);
  const [busy, setBusy] = useState(false);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const r = await fetch("/api/login", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ username: name, password }),
      });
      if (r.ok) {
        location.reload();
        return;
      }
      const body = await r.json().catch(() => ({}));
      setError(body.error ?? `The recorder said ${r.status}.`);
    } catch {
      setError("Can't reach the recorder.");
    }
    setBusy(false);
  }

  return (
    <div className="app">
      <div className="quit-screen login">
        <span className="logo" aria-hidden="true" />
        <h1>Trunk Recorder Pro</h1>
        {props.who.setup ? (
          <p className="muted">
            There are no accounts yet. On the recorder's own computer, open the interface and make an admin account in Setup → Accounts (or run{" "}
            <code>trunk-pro account add</code>), then log in here.
          </p>
        ) : (
          <form className="stack login-form" onSubmit={submit}>
            <label className="field">
              <span className="field-label">Name</span>
              <input autoFocus autoComplete="username" value={name} onChange={(e) => setName(e.target.value)} required />
            </label>
            <label className="field">
              <span className="field-label">Password</span>
              <input type="password" autoComplete="current-password" value={password} onChange={(e) => setPassword(e.target.value)} required />
            </label>
            {error && (
              <div className="banner bad" role="alert">
                {error}
              </div>
            )}
            <button className="btn primary" disabled={busy || !name || !password}>
              {busy ? "Logging in…" : "Log in"}
            </button>
          </form>
        )}
      </div>
    </div>
  );
}

/** Who the recorder takes us for, when it wants a login first; null when it lets us in. */
export async function needsLogin(): Promise<WhoAmI | null> {
  try {
    const r = await fetch("/api/whoami");
    return r.status === 401 ? ((await r.json()) as WhoAmI) : null;
  } catch {
    // Not reachable: the interface says so and keeps retrying.
    return null;
  }
}
