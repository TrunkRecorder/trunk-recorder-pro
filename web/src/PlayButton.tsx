// ▶ for a tone pattern: plays it here, in the browser; pressed again while it
// plays, stops it (some are long).

import { useEffect, useRef, useState } from "react";
import { play, schedule, type Playing, type ToneSpec } from "./tonePlayer.ts";

export function PlayButton({ spec }: { spec: ToneSpec }) {
  const [playing, setPlaying] = useState(false);
  const now = useRef<Playing | null>(null);
  // (Gone from the page: quiet.)
  useEffect(() => () => now.current?.stop(), []);
  const toggle = () => {
    if (now.current) return now.current.stop();
    const p = play(schedule(spec));
    now.current = p;
    setPlaying(true);
    void p.done.then(() => {
      now.current = null;
      setPlaying(false);
    });
  };
  return (
    <button type="button" className={`btn ghost small${playing ? " on" : ""}`} title={playing ? "Stop" : "Play it here"} aria-label={playing ? "Stop" : "Play"} onClick={toggle}>
      {playing ? "■" : "▶"}
    </button>
  );
}
