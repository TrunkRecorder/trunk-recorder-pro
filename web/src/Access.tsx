// Setup → Recording → Access (desktop app): signing in (server.auth). Off,
// whoever reaches a port gets its access; on, a browser signs in as a user,
// with their access token's secret or a passkey of theirs, and gets what the
// user's role allows: View (listen) or Admin (control). Turned on in an order
// that can't lock this browser out: an Admin user, this browser signed in as
// them, and only then on. Each user adds their own passkeys, signed in
// (where passkeys work: localhost, or server.hostname over HTTPS). A proxy
// in front that signs people in (server.auth.proxy) is its addresses and
// header, or Cloudflare Access's team and Audience tag, and what others get;
// its names mapped to users are a hand edit.

import { useState } from "react";
import { createToken, passkeySignIn, signIn, signOut, snapshot, subscribe, updateConfig, useSelect, type SignedIn } from "./controller.ts";
import { Info } from "./charts.tsx";
import { textList } from "./fieldText.ts";
import { CopyButton, DraftInput, Field, TextInput } from "./inputs.tsx";
import { Group, Section, Switch } from "./SetupLayout.tsx";
import { AddPasskey, day, PasskeyChip } from "./Passkeys.tsx";
import type { ApiAccess, AuthProxy, AuthSetup, AuthUser, Config } from "./protocol.ts";

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
 * Turn signing in on, an Admin user's access token `secret` in hand (or, with
 * `passkey`, one of their passkeys): this browser signed in as them (the
 * recorder says who and their role), the user in this browser's config (an
 * edit without them would take them out), and only then on. Why it wasn't, or null.
 */
export async function enableWith(secret: string, passkey = false): Promise<string | null> {
  const signed: SignedIn | string | null = passkey ? await passkeySignIn() : await signIn(secret);
  if (!signed) return "Not a token here";
  if (typeof signed === "string") return signed;
  const who = signed;
  if (who.access !== "control") {
    // (Its cookie out again: once on, it could be the one this browser is taken for.)
    await signOut(false);
    return passkey ? "Not an Admin's passkey" : "Not an Admin token";
  }
  if (!(await untilConfig((x) => x.server.auth?.users?.[who.user]?.access === "control"))) return "Token not saved yet";
  updateConfig((x) => void (authOf(x).enabled = true));
  return null;
}

/** A user just made: their token's secret, shown this once. */
export type Made = { name: string; access: ApiAccess; secret: string };

/** Whether they have something to sign in with: a token or a passkey. */
const canSignIn = (u: AuthUser) => !!u.token || !!u.passkeys?.length;
/** Who can turn signing in off again: an Admin with something to sign in with. */
const admin = (u: AuthUser) => u.access === "control" && canSignIn(u);

/** Addresses as typed: commas or spaces between. */
const addresses = textList(/[\s,]+/);

/** A proxy's draft: its kind, and what that kind is set up with. */
type ProxyDraft = { kind: "header" | "cloudflare"; from: string; header: string; team: string; aud: string };

/** The proxy in front (server.auth.proxy): set up as a header from its addresses, or Cloudflare Access (its team and Audience tag), then edited in place. */
function ProxyGroup(props: { proxy?: AuthProxy }) {
  const saved = useSelect((x) => ({ status: x.saveStatus, error: x.saveError }));
  const [draft, setDraft] = useState<ProxyDraft | null>(null);
  const [edited, setEdited] = useState(false);
  const p = props.proxy;
  const cf = p?.cloudflare;
  const edit = (f: (p: AuthProxy) => void) => {
    setEdited(true);
    updateConfig((x) => {
      const q = authOf(x).proxy;
      if (q) f(q);
    });
  };
  const ready = (d: ProxyDraft) => (d.kind === "cloudflare" ? !!d.team.trim() && !!d.aud.trim() : !!addresses.parse(d.from).length);
  const save = () => {
    if (!draft || !ready(draft)) return;
    setEdited(true);
    const d = draft;
    updateConfig(
      (x) =>
        void (authOf(x).proxy =
          d.kind === "cloudflare" ? { from: [], header: "Remote-User", cloudflare: { team: d.team.trim(), aud: d.aud.trim() } } : { from: addresses.parse(d.from), header: d.header.trim() || "Remote-User" }),
    );
    setDraft(null);
  };
  const remove = () => {
    if (!confirm("Remove the proxy? Whoever it signed in is signed out.")) return;
    setEdited(true);
    updateConfig((x) => void delete authOf(x).proxy);
  };
  const others = p?.other === "listen";
  const mapped = Object.keys(p?.users ?? {}).length;
  const kinds = draft && (
    <div className="field">
      <span className="field-label">Kind</span>
      <div className="seg small" role="radiogroup" aria-label="Proxy kind">
        {(["header", "cloudflare"] as const).map((k) => (
          <button key={k} type="button" role="radio" aria-checked={draft.kind === k} className={draft.kind === k ? "on" : ""} onClick={() => setDraft({ ...draft, kind: k })}>
            {k === "header" ? "Header" : "Cloudflare"}
          </button>
        ))}
      </div>
    </div>
  );
  return (
    <Group
      title="Proxy"
      hint={
        <>
          {cf ? "Cloudflare Access" : p ? "Signs people in" : "None"}
          <Info>
            A reverse proxy in front that signs people in (Authelia, oauth2-proxy, tailscale serve) and says who in a header: believed from its addresses alone. It must strip the
            header from what clients send. Or Cloudflare Access: who its signed token says, checked against the team&apos;s keys and the application&apos;s Audience tag, from any
            address. The name is the user of that name, with their role. Names that differ are mapped in the config file.
          </Info>
        </>
      }
      actions={
        p ? (
          <button type="button" className="btn ghost small danger" onClick={remove}>
            Remove
          </button>
        ) : (
          !draft && (
            <button type="button" className="btn small" onClick={() => setDraft({ kind: "header", from: "127.0.0.1, ::1", header: "Remote-User", team: "", aud: "" })}>
              Set up
            </button>
          )
        )
      }
      full
    >
      {draft && (
        <form
          className="row wide wrap"
          onSubmit={(e) => {
            e.preventDefault();
            save();
          }}
        >
          {kinds}
          {draft.kind === "header" ? (
            <>
              <Field label="From">
                <input className="mono" autoComplete="off" aria-label="Proxy addresses" value={draft.from} placeholder="127.0.0.1, 10.0.0.0/8" onChange={(e) => setDraft({ ...draft, from: e.target.value })} />
              </Field>
              <Field label="Header">
                <input className="mono" autoComplete="off" aria-label="Proxy header" value={draft.header} onChange={(e) => setDraft({ ...draft, header: e.target.value })} />
              </Field>
            </>
          ) : (
            <>
              <Field label="Team">
                <input className="mono" autoComplete="off" aria-label="Cloudflare team" value={draft.team} placeholder="myteam" onChange={(e) => setDraft({ ...draft, team: e.target.value })} />
              </Field>
              <Field label="Audience">
                <input className="mono" autoComplete="off" aria-label="Cloudflare audience" value={draft.aud} placeholder="AUD tag" onChange={(e) => setDraft({ ...draft, aud: e.target.value })} />
              </Field>
            </>
          )}
          <button className="btn primary" type="submit" disabled={!ready(draft)}>
            Save
          </button>
          <button type="button" className="btn ghost" onClick={() => setDraft(null)}>
            Cancel
          </button>
        </form>
      )}
      {p && (
        <div className="row wide wrap">
          {cf ? (
            <>
              <Field label="Team" hint="myteam.cloudflareaccess.com">
                <TextInput className="mono" aria-label="Cloudflare team" value={cf.team} onChange={(v) => edit((q) => void (q.cloudflare = { ...cf, ...q.cloudflare, team: v.trim() }))} />
              </Field>
              <Field label="Audience" hint="AUD tag">
                <TextInput className="mono" aria-label="Cloudflare audience" value={cf.aud} onChange={(v) => edit((q) => void (q.cloudflare = { ...cf, ...q.cloudflare, aud: v.trim() }))} />
              </Field>
            </>
          ) : (
            <>
              <Field label="From" hint="IPs or CIDR blocks">
                <DraftInput className="mono" aria-label="Proxy addresses" value={p.from} codec={addresses} onChange={(v) => edit((q) => void (q.from = v))} />
              </Field>
              <Field label="Header">
                <TextInput className="mono" aria-label="Proxy header" value={p.header ?? "Remote-User"} onChange={(v) => edit((q) => void (q.header = v || "Remote-User"))} />
              </Field>
            </>
          )}
          <div className="field">
            <span className="field-label">Others</span>
            <div className="seg small" role="radiogroup" aria-label="Others">
              <button type="button" role="radio" aria-checked={!others} className={others ? "" : "on"} onClick={() => edit((q) => void delete q.other)}>
                None
              </button>
              <button type="button" role="radio" aria-checked={others} className={others ? "on" : ""} onClick={() => edit((q) => void (q.other = "listen"))}>
                View
              </button>
            </div>
          </div>
          {mapped > 0 && (
            <span className="chip" title="Names mapped to users (the config file)">
              {mapped} mapped
            </span>
          )}
        </div>
      )}
      {edited && saved.status === "failed" && saved.error && <p className="bad-text">{saved.error}</p>}
    </Group>
  );
}

/** `made`: a user just made (tests). */
export function AccessSection(props: { c: Config; made?: Made }) {
  const saved = useSelect((x) => ({ status: x.saveStatus, error: x.saveError }));
  // Who this browser is signed in as, and whether passkeys work where it is.
  const me = useSelect((x) => ({ user: x.user, passkeys: x.passkeys }));
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

  const finish = async (secret: string, passkey = false) => {
    setBusy(true);
    setProblem(null);
    const why = await enableWith(secret, passkey);
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
  // The last Admin who can sign in, signing in on: they stay, and stay Admin, and keep a way in.
  const lastAdmin = (u: AuthUser) => on && admin(u) && admins <= 1;
  const keepsWayIn = (u: AuthUser, after: AuthUser) => canSignIn(after) && !(lastAdmin(u) && !admin(after));
  const removeToken = (name: string) => {
    if (!confirm(`Remove ${name}'s token? Browsers signed in with it are signed out; their passkeys stay.`)) return;
    updateConfig((x) => void delete authOf(x).users![name].token);
  };
  const removePasskey = (name: string, id: string, label: string) => {
    if (!confirm(`Remove ${name}'s passkey ${label}? Browsers signed in with it are signed out.`)) return;
    updateConfig((x) => void (authOf(x).users![name].passkeys = authOf(x).users![name].passkeys?.filter((p) => p.id !== id)));
  };
  const anon = auth?.anonymous === "listen";
  // Signing in with a passkey to turn it on: an Admin has one, and passkeys work here.
  const adminKeys = me.passkeys && rows.some(([, u]) => u.access === "control" && u.passkeys?.length);

  return (
    <Section title="Access" hint="Tokens, passkeys" actions={<Switch label="Sign-in" checked={on} disabled={busy} onChange={turn} />}>
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
              {adminKeys && (
                <button type="button" className="btn" disabled={busy} onClick={() => void finish("", true)}>
                  Passkey
                </button>
              )}
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
                <Info>View: sees and plays, changes nothing. Admin: everything. Each signs in with a token or a passkey of theirs; removing either signs their browsers out. Passkeys: added by each user, signed in, here.</Info>
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
                    <th>Token</th>
                    <th>Passkeys</th>
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
                      <td className="small">
                        {day(u.token?.created)}
                        {u.token && keepsWayIn(u, { ...u, token: undefined }) && (
                          <button type="button" className="btn ghost small danger" title="Remove" aria-label={`Remove ${name}'s token`} onClick={() => removeToken(name)}>
                            ×
                          </button>
                        )}
                      </td>
                      <td>
                        <div className="row wrap">
                          {(u.passkeys ?? []).map((p) => (
                            <PasskeyChip key={p.id} p={p}>
                              {keepsWayIn(u, { ...u, passkeys: u.passkeys!.filter((q) => q.id !== p.id) }) && (
                                <button type="button" className="btn ghost small danger" title="Remove" aria-label={`Remove ${name}'s passkey ${p.name}`} onClick={() => removePasskey(name, p.id, p.name)}>
                                  ×
                                </button>
                              )}
                            </PasskeyChip>
                          ))}
                          {!u.passkeys?.length && <span className="small">—</span>}
                          {me.passkeys && me.user === name && <AddPasskey />}
                        </div>
                      </td>
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
        <ProxyGroup proxy={auth?.proxy} />
      </section>
    </Section>
  );
}
