// Setup → Recording → Access (desktop app): signing in (server.auth). Off,
// whoever reaches a port gets its access; on, a browser signs in as a user,
// with their access token's secret, and gets what the user's role allows:
// View (listen) or Admin (control). Turned on in an order that can't lock
// this browser out: an Admin user, this browser signed in as them, and only
// then on.

import { useState } from "react";
import { createToken, signIn, signOut, snapshot, subscribe, updateConfig, useSelect } from "./controller.ts";
import { Info } from "./charts.tsx";
import { CopyButton, Field } from "./inputs.tsx";
import { Group, Section, Switch } from "./SetupLayout.tsx";
import type { ApiAccess, AuthSetup, AuthUser, Config } from "./protocol.ts";

/** The roles, as the interface calls them. */
export const ROLE: Record<ApiAccess, string> = { listen: "View", control: "Admin" };

/** Once this browser's config has `ok`: true; false after `ms` without. */
function untilConfig(ok: (c: Config) => boolean, ms = 5000): Promise<boolean> {
  return new Promise((resolve) => {
    const now = snapshot().config;
    if (now && ok(now)) return resolve(true);
    const off = subscribe(() => {
      const c = snapshot().config;
      if (!c || !ok(c)) return;
      clearTimeout(t);
      off();
      resolve(true);
    });
    const t = setTimeout(() => {
      off();
      resolve(false);
    }, ms);
  });
}

/** The config's server.auth, made if it has none. */
const authOf = (x: Config): AuthSetup => (x.server.auth ??= { enabled: false });

/**
 * Turn signing in on, an Admin user's access token `secret` in hand: this
 * browser signed in as them (the recorder says who and their role), the user
 * in this browser's config (an edit without them would take them out), and
 * only then on. Why it wasn't, or null.
 */
export async function enableWith(secret: string): Promise<string | null> {
  const who = await signIn(secret);
  if (!who) return "Not a token here";
  if (who.access !== "control") {
    // (Its cookie out again: once on, it could be the one this browser is taken for.)
    await signOut(false);
    return "Not an Admin token";
  }
  if (!(await untilConfig((x) => x.server.auth?.users?.[who.user]?.access === "control"))) return "Token not saved yet";
  updateConfig((x) => void (authOf(x).enabled = true));
  return null;
}

/** A user just made: their token's secret, shown this once. */
export type Made = { name: string; access: ApiAccess; secret: string };

/** Who can turn signing in off again: an Admin with something to sign in with. */
const admin = (u: AuthUser) => u.access === "control" && !!u.token;

/** `made`: a user just made (tests). */
export function AccessSection(props: { c: Config; made?: Made }) {
  const saved = useSelect((x) => ({ status: x.saveStatus, error: x.saveError }));
  const auth = props.c.server.auth;
  const users = auth?.users ?? {};
  const rows = Object.entries(users).sort(([, a], [, b]) => (a.token?.created ?? 0) - (b.token?.created ?? 0));
  const admins = rows.filter(([, u]) => admin(u)).length;
  const on = !!auth?.enabled;
  const [made, setMade] = useState<Made | null>(props.made ?? null);
  const [adding, setAdding] = useState<{ name: string; access: ApiAccess } | null>(null);
  // Turning on: an Admin user to make first, or the token secret of one made before (another browser, the command line).
  const [step, setStep] = useState<"admin" | "paste" | null>(null);
  const [paste, setPaste] = useState("");
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [enabling, setEnabling] = useState(false);

  const finish = async (secret: string) => {
    setBusy(true);
    setProblem(null);
    const why = await enableWith(secret);
    setBusy(false);
    if (why) return setProblem(why);
    setStep(null);
    setPaste("");
    setEnabling(true);
  };

  const turn = (v: boolean) => {
    setProblem(null);
    setEnabling(false);
    if (!v) {
      setStep(null);
      return updateConfig((x) => void (authOf(x).enabled = false));
    }
    if (made?.access === "control" && users[made.name]) return void finish(made.secret);
    if (admins) return setStep("paste");
    setStep("admin");
    setAdding({ name: adding?.name ?? "", access: "control" });
  };

  const create = async () => {
    if (!adding?.name.trim()) return;
    setBusy(true);
    setProblem(null);
    const r = await createToken(adding.name.trim(), adding.access);
    setBusy(false);
    if (r.type !== "tokenCreated" || !r.secret) return setProblem(r.type === "tokenCreated" ? (r.error ?? "Not made") : "No answer");
    const m = { name: r.name, access: adding.access, secret: r.secret };
    setMade(m);
    setAdding(null);
    if (step === "admin" && m.access === "control") void finish(m.secret);
  };

  const remove = (name: string) => {
    if (!confirm(`Remove ${name}? Browsers signed in as them are signed out.`)) return;
    updateConfig((x) => void delete authOf(x).users?.[name]);
    if (made?.name === name) setMade(null);
  };
  const setRole = (name: string, access: ApiAccess) => updateConfig((x) => void (authOf(x).users![name] = { ...authOf(x).users![name], access }));
  // The last Admin who can sign in, signing in on: they stay, and stay Admin.
  const lastAdmin = (u: AuthUser) => on && admin(u) && admins <= 1;
  const anon = auth?.anonymous === "listen";

  return (
    <Section title="Access" hint="Sign-in with tokens" actions={<Switch label="Sign-in" checked={on} disabled={busy} onChange={turn} />}>
      <section className="set-card">
        {step === "paste" && (
          <Group title="Sign in first" hint="An Admin token's secret" cols={1}>
            <form
              className="row"
              onSubmit={(e) => {
                e.preventDefault();
                if (paste.trim()) void finish(paste);
              }}
            >
              <Field label="Secret">
                <input type="password" className="mono" autoComplete="off" aria-label="Admin token secret" value={paste} onChange={(e) => setPaste(e.target.value)} />
              </Field>
              <button className="btn primary" type="submit" disabled={busy || !paste.trim()}>
                Sign in and turn on
              </button>
              <button
                type="button"
                className="btn ghost"
                onClick={() => {
                  setStep("admin");
                  setAdding({ name: "", access: "control" });
                }}
              >
                New Admin token
              </button>
              <button type="button" className="btn ghost" onClick={() => setStep(null)}>
                Cancel
              </button>
            </form>
          </Group>
        )}
        <Group title="Without a token" hint={on ? undefined : "Once sign-in is on"}>
          <div className="field wide">
            <div className="seg small" role="radiogroup" aria-label="Without a token">
              <button role="radio" aria-checked={!anon} className={anon ? "" : "on"} onClick={() => updateConfig((x) => void delete authOf(x).anonymous)}>
                None
              </button>
              <button role="radio" aria-checked={anon} className={anon ? "on" : ""} onClick={() => updateConfig((x) => void (authOf(x).anonymous = "listen"))}>
                View
              </button>
            </div>
          </div>
        </Group>
        <Group
          title="Users"
          hint={
            step === "admin" ? (
              <span className="warn-text">Admin token first</span>
            ) : (
              <>
                View or Admin
                <Info>View: sees and plays, changes nothing. Admin: everything. Each signs in with a token; removing one signs their browsers out.</Info>
              </>
            )
          }
          full
          actions={
            !adding && (
              <button className="btn small" onClick={() => setAdding({ name: "", access: "listen" })}>
                New token
              </button>
            )
          }
        >
          {made && (
            <div className="banner" role="status" aria-label="New token">
              <span>
                <b>{made.name}</b> <span className="chip">{ROLE[made.access]}</span>
              </span>
              <code className="mono secret">
                {made.secret}
              </code>
              <CopyButton text={made.secret} />
              <span className="warn-text small">Shown once</span>
              <button className="btn ghost small" onClick={() => setMade(null)}>
                Done
              </button>
            </div>
          )}
          {adding && (
            <form
              className="row wide"
              onSubmit={(e) => {
                e.preventDefault();
                void create();
              }}
            >
              <Field label="Name">
                <input autoComplete="off" maxLength={64} value={adding.name} placeholder="kitchen-tablet" onChange={(e) => setAdding({ ...adding, name: e.target.value })} />
              </Field>
              <Field label="Role">
                <select value={adding.access} disabled={step === "admin"} onChange={(e) => setAdding({ ...adding, access: e.target.value as ApiAccess })}>
                  <option value="listen">View</option>
                  <option value="control">Admin</option>
                </select>
              </Field>
              <button className="btn primary" type="submit" disabled={busy || !adding.name.trim()}>
                Create
              </button>
              <button
                type="button"
                className="btn ghost"
                onClick={() => {
                  setAdding(null);
                  if (step === "admin") setStep(null);
                }}
              >
                Cancel
              </button>
            </form>
          )}
          {rows.length ? (
            <div className="table-wrap wide">
              <table className="calls channels users">
                <thead>
                  <tr>
                    <th>Name</th>
                    <th>Role</th>
                    <th>Created</th>
                    <th />
                  </tr>
                </thead>
                <tbody>
                  {rows.map(([name, u]) => (
                    <tr key={name}>
                      <td className="mono">{name}</td>
                      <td>
                        <select aria-label={`Role of ${name}`} value={u.access} disabled={lastAdmin(u)} title={lastAdmin(u) ? "The last Admin" : undefined} onChange={(e) => setRole(name, e.target.value as ApiAccess)}>
                          <option value="listen">View</option>
                          <option value="control">Admin</option>
                        </select>
                      </td>
                      <td className="small">{u.token?.created ? new Date(u.token.created * 1000).toLocaleDateString() : "—"}</td>
                      <td>
                        {!lastAdmin(u) && (
                          <button type="button" className="btn ghost small danger" aria-label={`Remove ${name}`} onClick={() => remove(name)}>
                            Remove
                          </button>
                        )}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : (
            !adding && <p className="set-empty">No users</p>
          )}
          {problem && <p className="bad-text">{problem}</p>}
          {enabling && saved.status === "failed" && saved.error && <p className="bad-text">{saved.error}</p>}
        </Group>
      </section>
    </Section>
  );
}
