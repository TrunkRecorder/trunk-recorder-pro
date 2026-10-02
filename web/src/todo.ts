// What a Trunk Recorder import left to finish: which items are still open (the
// config, the dongles and the drivers say), what each asks, and the setup
// field it points at. An item closes by itself once its field is filled in.

import { formatMhz, resolvedCenters, sourceCovering, type ImportTodo } from "./config.ts";
import type { AppState } from "./controller.ts";

export interface OpenTodo {
  item: ImportTodo;
  /** The setup field it belongs to: its element is `need-<target>`; "plugins" and "plugin-<id>" are on Setup's Plugins tab. */
  target: string;
  title: string;
  text: string;
}

const DRIVER = { usrp: "UHD", airspy: "libairspy", soapy: "SoapySDR and your radio's module" };

export function openTodos(s: AppState): OpenTodo[] {
  const c = s.config;
  if (!c) return [];
  const out: OpenTodo[] = [];
  for (const item of s.todo) {
    switch (item.kind) {
      case "talkgroups": {
        const sys = c.systems.find((x) => x.shortName === item.system);
        if (sys && !sys.talkgroupsCsv)
          out.push({ item, target: `tg-${item.system}`, title: `Talkgroups for ${item.system}`, text: `“${item.file}” wasn't found. Load it to name the talkgroups.` });
        break;
      }
      case "units": {
        const sys = c.systems.find((x) => x.shortName === item.system);
        const names = sys?.unitNames ?? (c.conventional.shortName === item.system ? c.conventional.unitNames : undefined);
        if (!names?.csv)
          out.push({ item, target: `units-${item.system}`, title: `Unit names for ${item.system}`, text: `“${item.file}” (unitTagsFile) wasn't found. Load it to name the radios.` });
        break;
      }
      case "channels":
        if (!c.conventional.channelFile && c.conventional.channels.length <= item.had)
          out.push({ item, target: "channels", title: "Conventional channels", text: `“${item.file}” wasn't found. Link it here, or bring it in with Import CSV.` });
        break;
      case "siteLock": {
        const sys = c.systems.find((x) => x.shortName === item.system);
        if (sys && sys.expect.site == null)
          out.push({
            item,
            target: `site-${item.system}`,
            title: `Site lock for ${item.system}`,
            text: `Trunk Recorder followed only site ${item.siteId}. Fill in the site number — a first run shows the one the control channel announces.`,
          });
        break;
      }
      case "source": {
        const src = c.sources[item.index];
        if (!src || src.kind !== "rtlsdr" || src.serial !== item.serial) break;
        const dev = s.devices.find((d) => d.serial === item.serial);
        if (dev && !dev.busy) break;
        out.push({
          item,
          target: `src-${item.index}`,
          title: `Source ${item.index + 1}`,
          text: dev ? `Dongle ${item.serial} is ${dev.busy}.` : `Dongle ${item.serial} isn't plugged in. Plug it in, or choose another dongle.`,
        });
        break;
      }
      case "driver": {
        const src = c.sources[item.index];
        const d = s.radios?.[item.driver];
        if (!src || src.kind !== item.driver || !s.radios || d?.available) break;
        out.push({ item, target: `src-${item.index}`, title: `Source ${item.index + 1}`, text: `Needs ${DRIVER[item.driver]} installed on this computer, then a restart.` });
        break;
      }
      case "squelch":
        if (c.conventional.channels.length)
          out.push({ item, target: "squelch", title: "Squelch", text: "Here it's how far above the noise a channel must rise (default 8 dB), not Trunk Recorder's level. Check it once recording." });
        break;
      case "plugins":
        out.push({ item, target: "plugins", title: "Other plugins", text: `Trunk Recorder used ${item.names.join(", ")}, which has nothing like it here yet.` });
        break;
      case "plugin": {
        const p = s.plugins?.plugins.find((x) => x.id === item.id);
        // The web build has no plugins; the list arrives a moment after connecting.
        if (!s.plugins || (p && !p.problem && s.config?.plugins?.[item.id]?.enabled)) break;
        out.push({
          item,
          target: `plugin-${item.id}`,
          title: item.name,
          text:
            p && !p.problem
              ? "Its settings came over. Turn it on to start uploading."
              : `Its settings came over. Install ${item.name} on the Plugins page, then turn it on here.`,
        });
        break;
      }
      case "coverage": {
        const sys = c.systems.find((x) => x.shortName === item.system);
        if (!sys || !sys.enabled || !sys.controlChannels.length) break;
        const centers = resolvedCenters(c);
        if (sys.controlChannels.some((f) => sourceCovering(c, centers, f) >= 0)) break;
        out.push({
          item,
          target: `cc-${item.system}`,
          title: `Control channel of ${item.system}`,
          text: `No radio hears ${sys.controlChannels.map((f) => formatMhz(f, 4)).join(" or ")} MHz. Move a radio's center frequency there, or add a radio.`,
        });
        break;
      }
    }
  }
  return out;
}
