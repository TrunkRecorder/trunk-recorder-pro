// Passkeys, as a signed-in user sees their own: listed (the session's,
// any role), and one added for this browser. Setup → Access shows everyone's; the top bar, by the signed-in
// name, the user's own (any role: the server lets each add their own), and Sign out everywhere.
// Apps signed in by way of the browser (/app/sign-in) are listed beside them, and removed.

import { useEffect, useRef, useState, type ReactNode } from "react";
import { addPasskey, passkeyName, removeApp, signOut, signOutEverywhere, useSelect, type SignedInApp } from "./controller.ts";
import { Field } from "./inputs.tsx";
import type { ApiAccess, AuthPasskey } from "./protocol.ts";
import { confirmed } from "./asking.ts";

/** What a role is called here. */
const role = (a: ApiAccess) => (a === "control" ? "Admin" : "View");

/** A day from a Unix time, as this browser writes dates ("—": none). */
export const day = (t?: number) => (t ? new Date(t * 1000).toLocaleDateString() : "—");

/** A passkey: its name and the day it was added. `children`: what goes beside them (Remove). */
export function PasskeyChip(props: { p: Pick<AuthPasskey, "name" | "created">; children?: ReactNode }) {
  return (
    <span className="chip" title={`Added ${day(props.p.created)}`}>
      {props.p.name}
      <span className="small">{day(props.p.created)}</span>
      {props.children}
    </span>
  );
}

/** An app signed in by way of the browser: its name, what it may do, and Remove (its token stops working). */
export function AppChip(props: { a: SignedInApp }) {
  const [problem, setProblem] = useState<string | null>(null);
  const a = props.a;
  return (
    <span className="chip" title={problem ?? `Signed in ${day(a.created)} · Ends ${day(a.until)}`}>
      {a.name}
      <span className="small">{role(a.access)}</span>
      <button type="button" className="btn ghost small danger" title="Remove" aria-label={`Remove ${a.user}'s app ${a.name}`} onClick={async () => setProblem(await removeApp(a.id))}>
        ×
      </button>
      {problem && <span className="bad-text small">!</span>}
    </span>
  );
}

/** Add a passkey for the user this browser signed in as: its name (prefilled), then the browser makes it. */
export function AddPasskey() {
  const [naming, setNaming] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [added, setAdded] = useState<string | null>(null);
  const add = async () => {
    if (!naming?.trim()) return;
    setBusy(true);
    setProblem(null);
    const r = await addPasskey(naming.trim());
    setBusy(false);
    if (typeof r === "string") return setProblem(r);
    setNaming(null);
    setAdded(r.name);
  };
  return naming === null ? (
    <>
      {added && (
        <span className="chip" role="status" aria-label="Passkey added">
          Added <b>{added}</b>
        </span>
      )}
      <button
        type="button"
        className="btn small"
        onClick={() => {
          setAdded(null);
          setProblem(null);
          setNaming(passkeyName());
        }}
      >
        Add passkey
      </button>
    </>
  ) : (
    <form
      className="row wrap"
      onSubmit={(e) => {
        e.preventDefault();
        void add();
      }}
    >
      <Field label="Passkey name">
        <input autoComplete="off" maxLength={40} value={naming} aria-label="Passkey name" onChange={(e) => setNaming(e.target.value)} />
      </Field>
      <button className="btn primary small" type="submit" disabled={busy || !naming.trim()}>
        Add
      </button>
      <button type="button" className="btn ghost small" onClick={() => setNaming(null)}>
        Cancel
      </button>
      {problem && <span className="bad-text small">{problem}</span>}
    </form>
  );
}

/** Sign this user out everywhere (every browser's session of theirs, this one's too), asked first. */
function SignOutEverywhere() {
  const [problem, setProblem] = useState<string | null>(null);
  const go = async () => {
    const question = "Every browser and app signed in as you signs in again, this one too. Access tokens keep working.";
    if (!(await confirmed({ title: "Sign out everywhere?", question, action: "Sign out" }))) return;
    setProblem(await signOutEverywhere());
  };
  return (
    <div className="row wrap">
      <button type="button" className="btn ghost small danger" onClick={() => void go()}>
        Sign out everywhere
      </button>
      {problem && <span className="bad-text small">{problem}</span>}
    </div>
  );
}

/** What the signed-in name opens: where passkeys work, the user's passkeys (as the session lists them) and Add passkey; their apps, if any; Sign out everywhere (not when the proxy signed them in). */
export function UserPasskeys(props: { passkeys: boolean; proxied: boolean }) {
  const keys = useSelect((x) => x.ownPasskeys);
  const user = useSelect((x) => x.user);
  const apps = useSelect((x) => x.apps).filter((a) => a.user === user);
  return (
    <div className="user-pop" role="dialog" aria-label="Your sign-in">
      {props.passkeys && (
        <>
          <span className="field-label">Passkeys</span>
          {keys && (
            <ul className="user-keys">
              {keys.map((p) => (
                <li key={p.id} className="row">
                  <span>{p.name}</span>
                  <span className="muted small">{day(p.created)}</span>
                </li>
              ))}
              {!keys.length && <li className="muted small">None</li>}
            </ul>
          )}
          <div className="row wrap">
            <AddPasskey />
          </div>
        </>
      )}
      {!!apps.length && (
        <>
          <span className="field-label">Apps</span>
          <div className="row wrap">
            {apps.map((a) => (
              <AppChip key={a.id} a={a} />
            ))}
          </div>
        </>
      )}
      {!props.proxied && <SignOutEverywhere />}
    </div>
  );
}

/**
 * The top bar's signed-in name, its Sign out beside it (not when the proxy in
 * front signed them in: that's the proxy's), and the name opens the user's
 * passkeys (where passkeys work), their apps, and Sign out everywhere (not the proxy's).
 */
export function SignedInAs() {
  const s = useSelect((x) => ({ user: x.user, proxied: x.proxied, passkeys: x.passkeys, apps: x.apps.some((a) => a.user === x.user) }));
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => box.current && !box.current.contains(e.target as Node) && setOpen(false);
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("mousedown", away);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", away);
      document.removeEventListener("keydown", esc);
    };
  }, [open]);
  if (!s.user) return null;
  const title = s.proxied ? "Signed in by the proxy" : "Signed in as";
  const opens = s.passkeys || !s.proxied || s.apps;
  return (
    <span className="row user-menu" aria-label="Signed in" ref={box}>
      {opens ? (
        <button type="button" className="btn ghost small" title={title} aria-expanded={open} aria-haspopup="dialog" onClick={() => setOpen(!open)}>
          {s.user}
        </button>
      ) : (
        <span className="muted small" title={title}>
          {s.user}
        </span>
      )}
      {!s.proxied && (
        <button className="btn ghost small" onClick={() => void signOut()}>
          Sign out
        </button>
      )}
      {open && opens && <UserPasskeys passkeys={s.passkeys} proxied={s.proxied} />}
    </span>
  );
}
