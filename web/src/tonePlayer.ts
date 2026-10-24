// Tone patterns played in the browser (Web Audio), to hear a page or alert
// as it was heard or configured: its tones, at their lengths. What a pattern
// sounds like, step by step, is the recorder's (rules.ts toneSteps).

import { audioOut } from "./audioOut.ts";
import type { TonePatternKind } from "./protocol.ts";
import { toneSteps, type Step } from "./rules.ts";

/** What to play: a heard pattern (the tones log's row, the call JSON's) or a signal typed in. */
export type ToneSpec = { pattern: TonePatternKind; tones: number[]; lengths: number[]; digits?: string; knox?: boolean; cycles?: number };

/** Something playing: `done` when it's over (played out or stopped), `stop` to end it now. */
export type Playing = { done: Promise<void>; stop: () => void };

/** Play `spec`. */
export function play(spec: ToneSpec): Playing {
  const steps: Step[] = toneSteps(spec);
  let over = false;
  let silence: (() => void) | null = null;
  let resolve = () => {};
  const done = new Promise<void>((r) => (resolve = r));
  const stop = () => {
    if (over) return;
    over = true;
    silence?.();
    resolve();
  };
  void (async () => {
    const ac = audioOut();
    if (ac.state === "suspended") await ac.resume();
    if (over) return;
    const ramp = 0.005;
    // Everything goes through one gain: stopping fades it out over 20 ms, then cuts it off.
    const out = ac.createGain();
    out.connect(ac.destination);
    silence = () => {
      out.gain.setValueAtTime(out.gain.value, ac.currentTime);
      out.gain.linearRampToValueAtTime(0, ac.currentTime + 0.02);
      setTimeout(() => out.disconnect(), 50);
    };
    let t = ac.currentTime + 0.05;
    for (const st of steps) {
      for (const f of st.hz) {
        const o = ac.createOscillator();
        const g = ac.createGain();
        o.frequency.value = f;
        // Fade in and out over 5 ms: a step's edges don't click.
        g.gain.setValueAtTime(0, t);
        g.gain.linearRampToValueAtTime(0.25 / st.hz.length, t + ramp);
        g.gain.setValueAtTime(0.25 / st.hz.length, t + st.s - ramp);
        g.gain.linearRampToValueAtTime(0, t + st.s);
        o.connect(g).connect(out);
        o.start(t);
        o.stop(t + st.s);
      }
      t += st.s;
    }
    setTimeout(stop, (t - ac.currentTime) * 1000);
  })();
  return { done, stop };
}
