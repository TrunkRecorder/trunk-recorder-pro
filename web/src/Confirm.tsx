// The dialog that asks before something is done, and the app's one place it shows a question asked
// with `confirmed` (asking.ts).

import { useSyncExternalStore, type ReactNode } from "react";
import { answer, asking, subscribeAsking, type Ask } from "./asking.ts";
import { Modal } from "./Identify.tsx";

/**
 * Cancel or `action`, Cancel the default (Enter, Escape or a click outside). `children`: more to
 * choose before saying yes (RemoveSystem's "Also delete its data").
 */
export function Confirm(props: Ask & { children?: ReactNode; onYes: () => void; onClose: () => void }) {
  return (
    <Modal title={props.title} onClose={props.onClose}>
      {props.question?.split("\n\n").map((p, i) => <p key={i}>{p}</p>)}
      {props.children}
      <div className="row">
        <span className="spacer" />
        <button className="btn ghost" onClick={props.onClose} autoFocus>
          Cancel
        </button>
        <button className={props.safe ? "btn" : "btn danger"} onClick={props.onYes}>
          {props.action}
        </button>
      </div>
    </Modal>
  );
}

/** The question asked with `confirmed`, over whatever page is showing. */
export function Asking() {
  const a = useSyncExternalStore(subscribeAsking, asking, asking);
  return a && <Confirm key={a.id} {...a} onYes={() => answer(true)} onClose={() => answer(false)} />;
}
