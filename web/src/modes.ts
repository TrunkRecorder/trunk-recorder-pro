// What each of a conventional channel's modes is, in one place (the recorder's ConvMode has the same
// questions as methods: takes_code, matches_talkgroup, air_names_talkgroup, has_fm, has_analog_receiver).

import type { Channel } from "./protocol.ts";

export type Mode = Channel["mode"];

export interface ModeInfo {
  /** In Setup's menus. */
  label: string;
  /** On the dashboard. */
  short: string;
  /** In a message ("AM channels carry no tone or code"). */
  name: string;
  /** Rows on a frequency are told apart by a tone or code: not AM's, nor auto's, which has none to give. */
  takesCode: boolean;
  /** A transmission names its talkgroup, which a row can match, and AutoTune can use: the digital modes. */
  matchesTalkgroup: boolean;
  /** A call's talkgroup may be the air's: the digital modes, and auto when it settles on one. */
  airNamesTalkgroup: boolean;
  /** FM or AM, and nothing else: it has a bandwidth of its own. */
  analog: boolean;
  /** Has an analog receiver, so the Demodulation settings apply: analog, and auto's. */
  analogReceiver: boolean;
  /** Has an FM receiver (de-emphasis). */
  hasFm: boolean;
  /** The code field: its placeholder, and what it takes. */
  code: { placeholder: string; title: string };
  /** What a row with no code is called. */
  none: string;
  /** A code, as a chip says it. */
  chip: (code: string) => string;
  /** The row a frequency's "+" adds, where there is one: what it is told apart by, and the button. */
  add?: { by: string; button: string };
}

const nxdn = (label: string, short: string): ModeInfo => ({
  label,
  short,
  name: "NXDN",
  takesCode: true,
  matchesTalkgroup: true,
  airNamesTalkgroup: true,
  analog: false,
  analogReceiver: false,
  hasFm: false,
  code: { placeholder: "any RAN", title: "RAN (5). Empty or 0: any. RAN 5 TG 201 fills Match TG too." },
  none: "no code",
  chip: (code) => `RAN ${code}`,
  add: { by: "RAN or talkgroup", button: "+ rule" },
});

/** In the order Setup's menus list them. */
export const MODES: Record<Mode, ModeInfo> = {
  fm: {
    label: "Analog FM",
    short: "FM",
    name: "FM",
    takesCode: true,
    matchesTalkgroup: false,
    airNamesTalkgroup: false,
    analog: true,
    analogReceiver: true,
    hasFm: true,
    code: { placeholder: "any", title: "CTCSS tone or DCS code (151.4 PL, 023 DPL). Empty: any." },
    none: "no tone",
    chip: (code) => (/^\d/.test(code) ? `${code} Hz` : code),
    add: { by: "tone", button: "+ tone" },
  },
  am: {
    label: "Analog AM",
    short: "AM",
    name: "AM",
    takesCode: false,
    matchesTalkgroup: false,
    airNamesTalkgroup: false,
    analog: true,
    analogReceiver: true,
    hasFm: false,
    code: { placeholder: "—", title: "AM carries no tone or code." },
    none: "no code",
    chip: (code) => code,
  },
  p25: {
    label: "P25",
    short: "P25",
    name: "P25",
    takesCode: true,
    matchesTalkgroup: true,
    airNamesTalkgroup: true,
    analog: false,
    analogReceiver: false,
    hasFm: false,
    code: { placeholder: "any NAC", title: "NAC, hex (293). Empty: any. NAC 293 TG 1 fills Match TG too." },
    none: "no NAC",
    chip: (code) => `NAC ${code}`,
    add: { by: "NAC or talkgroup", button: "+ rule" },
  },
  dmr: {
    label: "DMR",
    short: "DMR",
    name: "DMR",
    takesCode: true,
    matchesTalkgroup: true,
    airNamesTalkgroup: true,
    analog: false,
    analogReceiver: false,
    hasFm: false,
    code: { placeholder: "any CC", title: "Colour code (1). Empty: any. RadioReference's CC1 TS2 TG201 fills Slot and Match TG too." },
    none: "no code",
    chip: (code) => `CC ${code}`,
    add: { by: "colour code, slot or talkgroup", button: "+ rule" },
  },
  nxdn48: nxdn("NXDN48 (6.25 kHz)", "NXDN 4800"),
  nxdn96: nxdn("NXDN96 (12.5 kHz)", "NXDN 9600"),
  auto: {
    label: "Auto (any of these but AM)",
    short: "Auto",
    name: "auto",
    takesCode: false,
    matchesTalkgroup: false,
    airNamesTalkgroup: true,
    analog: false,
    analogReceiver: true,
    hasFm: true,
    code: {
      placeholder: "—",
      title:
        "Auto has no tone or code: one row records everything on the frequency. Every receiver listens until one hears its own protocol (digital); analog is settled on after about two seconds. The call is the one it settled on. FM on a channel known to be analog doesn't wait.",
    },
    none: "no code",
    chip: (code) => code,
  },
};
