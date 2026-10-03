// Accounts (desktop app): the Setup tab where admins add and manage them,
// and the menu where anyone changes their own password or logs out.

import { useEffect, useState } from "react";
import { addAccount, changePassword, fetchAccounts, logOut, removeAccount, setAccountPassword, setAccountRole, setNotice, useApp } from "./controller.ts";
import type { Role } from "./protocol.ts";
import { setTheme, THEMES, useTheme } from "./theme.ts";

const MIN_PASSWORD = 8;

export function AccountsPanel() {
  const s = useApp();
  const open = s.access && !s.access.accounts;
  useEffect(() => {
    if (!open) fetchAccounts();
  }, [open]);

  if (open) return <FirstAdmin />;
  return (
    <>
      <section className="panel">
        <header className="panel-head">
          <h2>Accounts</h2>
          <span className="muted small">Admins set up and run the recorder. Viewers watch and listen.</span>
        </header>
        <table className="accounts-table">
          <thead>
            <tr>
              <th>Name</th>
              <th>Role</th>
              <th>Logged in</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {(s.accounts ?? []).map((a) => (
              <AccountRow key={a.name} name={a.name} role={a.role} sessions={a.sessions} me={a.name === s.access?.user} />
            ))}
          </tbody>
        </table>
      </section>
      <AddAccount />
    </>
  );
}

function AccountRow(props: { name: string; role: Role; sessions: number; me: boolean }) {
  const [password, setPassword] = useState<string | null>(null);
  return (
    <tr>
      <td>
        {props.name}
        {props.me && <span className="muted small"> (you)</span>}
      </td>
      <td>
        <select value={props.role} onChange={(e) => setAccountRole(props.name, e.target.value as Role)}>
          <option value="admin">Admin</option>
          <option value="viewer">Viewer</option>
        </select>
      </td>
      <td className="muted">{props.sessions === 0 ? "—" : props.sessions === 1 ? "1 session" : `${props.sessions} sessions`}</td>
      <td>
        <div className="row">
          {password === null ? (
            <button className="btn ghost small" onClick={() => setPassword("")}>
              New password
            </button>
          ) : (
            <form
              className="row"
              onSubmit={(e) => {
                e.preventDefault();
                setAccountPassword(props.name, password);
                setPassword(null);
              }}
            >
              <input
                type="password"
                autoComplete="new-password"
                autoFocus
                placeholder={`At least ${MIN_PASSWORD} characters`}
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
              <button className="btn small" disabled={password.length < MIN_PASSWORD}>
                Set
              </button>
              <button type="button" className="btn ghost small" onClick={() => setPassword(null)}>
                Cancel
              </button>
            </form>
          )}
          {!props.me && (
            <button className="btn ghost small" onClick={() => confirm(`Remove ${props.name}? They're logged out at once.`) && removeAccount(props.name)}>
              Remove
            </button>
          )}
        </div>
      </td>
    </tr>
  );
}

function AddAccount() {
  const [name, setName] = useState("");
  const [role, setRole] = useState<Role>("viewer");
  const [password, setPassword] = useState("");
  return (
    <section className="panel">
      <header className="panel-head">
        <h2>Add an account</h2>
      </header>
      <form
        className="grid2"
        onSubmit={(e) => {
          e.preventDefault();
          addAccount(name.trim(), role, password);
          setName("");
          setPassword("");
        }}
      >
        <label className="field">
          <span className="field-label">Name</span>
          <input autoComplete="off" value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <label className="field">
          <span className="field-label">Role</span>
          <select value={role} onChange={(e) => setRole(e.target.value as Role)}>
            <option value="viewer">Viewer: watches and listens</option>
            <option value="admin">Admin: sets up and runs the recorder</option>
          </select>
        </label>
        <label className="field">
          <span className="field-label">Password</span>
          <input type="password" autoComplete="new-password" value={password} onChange={(e) => setPassword(e.target.value)} />
          <span className="field-hint">At least {MIN_PASSWORD} characters. Tell it to them; they can change it after logging in.</span>
        </label>
        <div className="field">
          <span className="field-label">&nbsp;</span>
          <button className="btn primary" disabled={!name.trim() || password.length < MIN_PASSWORD}>
            Add
          </button>
        </div>
      </form>
    </section>
  );
}

/** No accounts yet: this computer only. Making the first admin opens the recorder to logins. */
function FirstAdmin() {
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [again, setAgain] = useState("");
  const [error, setError] = useState<string | null>(null);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (password !== again) {
      setError("The passwords don't match.");
      return;
    }
    const r = await fetch("/api/setup", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ username: name.trim(), password }),
    }).catch(() => null);
    if (r?.ok) {
      // Logged in as the new admin; the connection starts again with it.
      location.reload();
      return;
    }
    const body = await r?.json().catch(() => ({}));
    setError(body?.error ?? "The recorder didn't take it.");
  }

  return (
    <section className="panel">
      <header className="panel-head">
        <h2>Accounts</h2>
      </header>
      <p className="muted">
        There are no accounts, so only this computer can open the recorder. To open it to others (with <code>--bind 0.0.0.0</code>, or behind a proxy), make an
        admin account first. Then add viewers, who can watch and listen but change nothing.
      </p>
      <form className="grid2" onSubmit={submit}>
        <label className="field">
          <span className="field-label">Admin name</span>
          <input autoComplete="username" value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <span />
        <label className="field">
          <span className="field-label">Password</span>
          <input type="password" autoComplete="new-password" value={password} onChange={(e) => setPassword(e.target.value)} />
          <span className="field-hint">At least {MIN_PASSWORD} characters</span>
        </label>
        <label className="field">
          <span className="field-label">Again</span>
          <input type="password" autoComplete="new-password" value={again} onChange={(e) => setAgain(e.target.value)} />
        </label>
        {error && <div className="banner bad wide">{error}</div>}
        <div className="field">
          <button className="btn primary" disabled={!name.trim() || password.length < MIN_PASSWORD}>
            Make the admin account
          </button>
        </div>
      </form>
    </section>
  );
}

/** The top bar's menu: the theme, and with accounts, who's logged in, their password and Log out. */
export function UserMenu() {
  const s = useApp();
  const { theme } = useTheme();
  const [old, setOld] = useState("");
  const [password, setPassword] = useState("");
  const [again, setAgain] = useState("");
  const accounts = !!s.access?.accounts;
  const ok = old && password.length >= MIN_PASSWORD && password === again;
  return (
    <details className="user-menu">
      <summary className="btn ghost" title={accounts ? "Your account and the theme" : "Theme"}>
        {accounts ? (
          <>
            {s.access!.user}
            <span className="muted small"> · {s.access!.role}</span>
          </>
        ) : (
          "Theme"
        )}
      </summary>
      <div className="menu stack">
        <label className="field">
          <span className="field-label">Theme (this browser)</span>
          <select value={theme} onChange={(e) => setTheme(e.target.value)}>
            {THEMES.map((t) => (
              <option key={t.id} value={t.id}>
                {t.name}
              </option>
            ))}
          </select>
        </label>
        {accounts && (
          <>
            <form
              className="stack"
              onSubmit={(e) => {
                e.preventDefault();
                changePassword(old, password);
                setOld("");
                setPassword("");
                setAgain("");
                setNotice(null);
              }}
            >
              <strong>Change your password</strong>
              <input type="password" autoComplete="current-password" placeholder="Current password" value={old} onChange={(e) => setOld(e.target.value)} />
              <input
                type="password"
                autoComplete="new-password"
                placeholder={`New (${MIN_PASSWORD}+ characters)`}
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
              <input type="password" autoComplete="new-password" placeholder="New, again" value={again} onChange={(e) => setAgain(e.target.value)} />
              <button className="btn small" disabled={!ok}>
                Change
              </button>
            </form>
            <button className="btn ghost" onClick={logOut}>
              Log out
            </button>
          </>
        )}
      </div>
    </details>
  );
}
