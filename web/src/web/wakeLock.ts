// The web build records in this tab, and phones and tablets freeze a tab once
// its screen sleeps. Holding a screen wake lock while recording keeps it
// going (as long as the tab stays in front).

import { useEffect } from "react";

/** Hold a screen wake lock while `on`. The browser drops it whenever the tab
 *  is hidden, so take it again each time the tab comes back. */
export function useWakeLock(on: boolean): void {
  useEffect(() => {
    if (!on || !("wakeLock" in navigator)) return;
    let lock: WakeLockSentinel | null = null;
    let done = false;
    const take = async () => {
      if (done || document.visibilityState !== "visible" || (lock && !lock.released)) return;
      try {
        const l = await navigator.wakeLock.request("screen");
        if (done) void l.release();
        else lock = l;
      } catch {
        /* refused (battery saver, no user gesture yet): try again on the next visibility change */
      }
    };
    void take();
    document.addEventListener("visibilitychange", take);
    return () => {
      done = true;
      document.removeEventListener("visibilitychange", take);
      void lock?.release();
    };
  }, [on]);
}
