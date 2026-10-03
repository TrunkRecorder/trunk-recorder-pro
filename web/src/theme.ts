// Colour themes, chosen in each browser. "auto" follows the system's light or
// dark setting; the others are palettes in styles.css (:root[data-theme=…]).

import { useSyncExternalStore } from "react";

export const THEMES: { id: string; name: string }[] = [
  { id: "auto", name: "Automatic" },
  { id: "dark", name: "Dark" },
  { id: "light", name: "Light" },
  { id: "nostromo", name: "Nostromo" },
  { id: "classic", name: "Classic" },
  { id: "hotdog", name: "Hot Dog Stand" },
];

const KEY = "trp.theme";

function stored(): string {
  try {
    const t = localStorage.getItem(KEY);
    if (t && THEMES.some((x) => x.id === t)) return t;
  } catch {
    // Not remembered.
  }
  return "auto";
}

let current = stored();
/** Bumped whenever the colours change. */
let version = 0;
const listeners = new Set<() => void>();
function changed(): void {
  version++;
  listeners.forEach((l) => l());
}

function apply(): void {
  if (current === "auto") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = current;
}
apply();
// "auto" changes with the system's setting.
matchMedia("(prefers-color-scheme: light)").addEventListener("change", () => current === "auto" && changed());

export function setTheme(id: string): void {
  current = THEMES.some((x) => x.id === id) ? id : "auto";
  try {
    localStorage.setItem(KEY, current);
  } catch {
    // This session only.
  }
  apply();
  changed();
}

/** The theme chosen, and a counter that changes whenever the colours do (for canvases). */
export function useTheme(): { theme: string; epoch: number } {
  const epoch = useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => version,
  );
  return { theme: current, epoch };
}
