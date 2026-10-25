// The page's one AudioContext, shared by everything that plays (live calls,
// tone patterns). It is made on first use, after a click (browsers won't
// start one before), and kept for the page's life.
//
// A context can drop out of "running" (Safari: tab switches, output device
// changes) and only resume on a user gesture, so any click or key press on
// the page resumes it.

let ctx: AudioContext | null = null;

/** The page's AudioContext, made now if it isn't yet, and asked to run. */
export function audioOut(): AudioContext {
  if (!ctx) {
    ctx = new AudioContext();
    const wake = () => {
      if (ctx && ctx.state !== "running") void ctx.resume().catch(() => {});
    };
    document.addEventListener("pointerdown", wake, true);
    document.addEventListener("keydown", wake, true);
    document.addEventListener("visibilitychange", wake);
  }
  if (ctx.state !== "running") void ctx.resume().catch(() => {});
  return ctx;
}
