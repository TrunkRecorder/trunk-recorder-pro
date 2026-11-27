// Asking before something is done (removed, replaced, quit): one dialog for the whole app, in place
// of the browser's confirm(), whose default is OK. Here Cancel is the default (Enter, Escape or a
// click outside). Confirm.tsx's Asking shows it; this module has no interface of its own, so the
// controller can ask too.

/** A question: `title` asks it, `question` says what follows, `action` is the button that does it. */
export interface Ask {
  title: string;
  question?: string;
  action: string;
  /** Nothing is lost by it (going on with a setting, giving access): the button isn't marked as dangerous. */
  safe?: boolean;
}

/** A question asked, waiting for its answer. */
export interface Asked extends Ask {
  id: number;
  answer: (yes: boolean) => void;
}

let queue: Asked[] = [];
let next = 0;
const listeners = new Set<() => void>();

/** Whether the user says yes to `ask`. One question at a time: one asked meanwhile waits its turn. */
export function confirmed(ask: Ask): Promise<boolean> {
  return new Promise((answer) => {
    queue = [...queue, { ...ask, id: ++next, answer }];
    listeners.forEach((l) => l());
  });
}

/** The question showing now, if any. */
export function asking(): Asked | null {
  return queue[0] ?? null;
}

/** The answer to the question showing now. */
export function answer(yes: boolean): void {
  const [first, ...rest] = queue;
  if (!first) return;
  queue = rest;
  listeners.forEach((l) => l());
  first.answer(yes);
}

export function subscribeAsking(l: () => void): () => void {
  listeners.add(l);
  return () => void listeners.delete(l);
}
