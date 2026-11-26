// A conventional channel's modes: what a row of each can say is the recorder's (ConvMode, through
// rules.ts: convModes); how Setup and the dashboard show it is here.

import type { Channel } from "./protocol.ts";
import { convModes, toneLabel, type ModeFacts } from "./rules.ts";

export type Mode = Channel["mode"];

/** How a mode is shown. */
interface ModeLook {
  /** In Setup's menus. */
  label: string;
  /** On the dashboard. */
  short: string;
  /** The code field: its placeholder, and what it takes. */
  code: { placeholder: string; title: string };
  /** What a row with no code is called. */
  none: string;
  /** A code, as a chip says it. */
  chip: (code: string) => string;
  /** The button that adds a row on its frequency (a mode with a code to tell them apart by). */
  addButton?: string;
}

export type ModeInfo = ModeFacts & ModeLook;

const NO_CODE = { placeholder: "—", title: "" };
const ran = { placeholder: "any RAN", title: "RAN (5). Empty or 0: any. RAN 5 TG 201 fills Match TG too." };

const LOOK: Record<Mode, ModeLook> = {
  fm: {
    label: "Analog FM",
    short: "FM",
    code: { placeholder: "any", title: "CTCSS tone or DCS code (151.4 PL, 023 DPL). Empty: any." },
    none: "no tone",
    chip: (code) => toneLabel(code).text,
    addButton: "+ tone",
  },
  am: { label: "Analog AM", short: "AM", code: { ...NO_CODE, title: "AM carries no tone or code." }, none: "no code", chip: (code) => code },
  p25: {
    label: "P25",
    short: "P25",
    code: { placeholder: "any NAC", title: "NAC, hex (293). Empty: any. NAC 293 TG 1 fills Match TG too." },
    none: "no NAC",
    chip: (code) => `NAC ${code}`,
    addButton: "+ rule",
  },
  dmr: {
    label: "DMR",
    short: "DMR",
    code: { placeholder: "any CC", title: "Colour code (1). Empty: any. RadioReference's CC1 TS2 TG201 fills Slot and Match TG too." },
    none: "no code",
    chip: (code) => `CC ${code}`,
    addButton: "+ rule",
  },
  nxdn48: { label: "NXDN48 (6.25 kHz)", short: "NXDN 4800", code: ran, none: "no code", chip: (code) => `RAN ${code}`, addButton: "+ rule" },
  nxdn96: { label: "NXDN96 (12.5 kHz)", short: "NXDN 9600", code: ran, none: "no code", chip: (code) => `RAN ${code}`, addButton: "+ rule" },
  auto: {
    label: "Auto (any of these but AM)",
    short: "Auto",
    code: {
      ...NO_CODE,
      title:
        "Auto has no tone or code: one row records everything on the frequency. Every receiver listens until one hears its own protocol (digital); analog is settled on after about two seconds. The call is the one it settled on. FM on a channel known to be analog doesn't wait.",
    },
    none: "no code",
    chip: (code) => code,
  },
};

let modes: Map<Mode, ModeInfo> | undefined;

/** Every mode, in the order Setup's menus list them (the recorder's). */
export function allModes(): ModeInfo[] {
  modes ??= new Map(convModes().map((f) => [f.mode, { ...f, ...LOOK[f.mode] }]));
  return [...modes.values()];
}

/** What mode `m` is, and how it is shown. */
export function modeInfo(m: Mode): ModeInfo {
  allModes();
  return modes!.get(m)!;
}
