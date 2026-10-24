// Tone patterns played in the browser (Web Audio), to hear a page or alert
// as it was heard or configured: its tones, at their lengths.

import type { TonePatternKind } from "./protocol.ts";

/** One step: these frequencies together (none: silence), for `s` seconds. */
export type Step = { hz: number[]; s: number };

/** What to play: a heard row or a configured tone. */
export type ToneSpec = { pattern: TonePatternKind; tones: number[]; lengths: number[]; digits?: string; cycles?: number };

const DTMF_ROWS = [697, 770, 852, 941];
const DTMF_COLS = [1209, 1336, 1477, 1633];
const KNOX_ROWS = [606, 672, 743, 820];
const KNOX_COLS = [1052, 1162, 1279, 1430];
const KEYS = ["123A", "456B", "789C", "*0#D"];

/** Morse, letters and figures. */
const MORSE: Record<string, string> = {
  A: ".-", B: "-...", C: "-.-.", D: "-..", E: ".", F: "..-.", G: "--.", H: "....", I: "..", J: ".---", K: "-.-", L: ".-..", M: "--",
  N: "-.", O: "---", P: ".--.", Q: "--.-", R: ".-.", S: "...", T: "-", U: "..-", V: "...-", W: ".--", X: "-..-", Y: "-.--", Z: "--..",
  "0": "-----", "1": ".----", "2": "..---", "3": "...--", "4": "....-", "5": ".....", "6": "-....", "7": "--...", "8": "---..", "9": "----.", "/": "-..-.", "=": "-...-",
};

/** Text keyed as Morse on `hz` with a dot of `dot` s (a dash 3, gaps 1, 3 and 7). */
export function morseSteps(text: string, hz: number, dot: number): Step[] {
  const out: Step[] = [];
  for (const w of text.trim().split(/\s+/)) {
    if (out.length) out.push({ hz: [], s: 7 * dot });
    for (const c of w.toUpperCase()) {
      const code = MORSE[c];
      if (!code) continue;
      if (out.length && out[out.length - 1].hz.length) out.push({ hz: [], s: 3 * dot });
      [...code].forEach((e, i) => {
        if (i) out.push({ hz: [], s: dot });
        out.push({ hz: [hz], s: e === "-" ? 3 * dot : dot });
      });
    }
  }
  return out;
}

/** A key's two frequencies on its pad (KNOX's or DTMF's). */
export function keyHz(c: string, knox: boolean): number[] {
  const r = KEYS.findIndex((row) => row.includes(c));
  if (r < 0) return [];
  const k = KEYS[r].indexOf(c);
  return knox ? [KNOX_ROWS[r], KNOX_COLS[k]] : [DTMF_ROWS[r], DTMF_COLS[k]];
}

/** Longest a played tone runs, s (an 8 s group call is heard well enough in 4). */
const MAX_TONE_S = 4;

/** The steps a pattern plays as. Lengths not known: the usual (a 1 s + 3 s page, 0.1 s digits). */
export function schedule(p: ToneSpec): Step[] {
  const len = (k: number, d: number) => Math.min(MAX_TONE_S, p.lengths[k] > 0 ? p.lengths[k] : d);
  const [a, b] = p.tones;
  switch (p.pattern) {
    case "two_tone":
      return [{ hz: [a], s: len(0, 1) }, { hz: [b], s: len(1, 3) }];
    case "long":
    case "beep":
      return [{ hz: [a], s: len(0, p.pattern === "long" ? 3 : 0.2) }];
    case "hi_low":
      return Array.from({ length: Math.min(p.cycles || 12, 24) }, (_, i) => ({ hz: [i % 2 ? b : a], s: len(i % 2, 0.25) }));
    case "pulsed":
      return Array.from({ length: Math.min(p.cycles || 6, 12) }).flatMap(() => [{ hz: [a], s: len(0, 0.3) }, { hz: [], s: len(1, 0.2) }]);
    case "sequence":
      return p.tones.map((f, i) => ({ hz: [f], s: len(i, 0.07) }));
    case "morse":
      return morseSteps(p.digits ?? "", a || 1000, len(0, 0.06));
    case "digits": {
      const knox = (p.digits ?? "").startsWith("KNOX ");
      return [...(p.digits ?? "").replace(/^KNOX /, "")].flatMap((c, i) => [{ hz: keyHz(c, knox), s: len(i, 0.1) }, { hz: [], s: 0.1 }]);
    }
  }
}

let ctx: AudioContext | null = null;

/** Something playing: `done` when it's over (played out or stopped), `stop` to end it now. */
export type Playing = { done: Promise<void>; stop: () => void };

/** Play `steps`. */
export function play(steps: Step[]): Playing {
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
    ctx ??= new AudioContext();
    if (ctx.state === "suspended") await ctx.resume();
    if (over) return;
    const ac = ctx;
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
