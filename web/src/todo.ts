// What's waiting on you: what a Trunk Recorder import left to finish (which items
// are still open, as the config, the dongles and the drivers say, each closing by
// itself once its field is filled in), plugin changes held until Apply, and
// changes that wait for a restart. Each says what it asks and where it is.

import { formatMhz, receiverOf, siteNameOf, SOURCE_KINDS, sourceTitle, type ImportTodo } from "./config.ts";
import { isRecording, systemAt, type AppState, type HeldForm } from "./controller.ts";

export interface OpenTodo {
  item: ImportTodo;
  /** The setup field it belongs to: its element is `need-<target>` (a site's: `<kind>-<system>-<site index>`); "plugins" and "plugin-<id>" are on Setup's Plugins tab. */
  target: string;
  title: string;
  text: string;
}


export function openTodos(s: AppState): OpenTodo[] {
  const c = s.config;
  if (!c) return [];
  const out: OpenTodo[] = [];
  for (const item of s.todo) {
    switch (item.kind) {
      case "talkgroups": {
        const sys = c.systems.find((x) => x.shortName === item.system);
        if (sys && !s.talkgroups[sys.shortName])
          out.push({ item, target: `tg-${item.system}`, title: `Talkgroups for ${item.system}`, text: `“${item.file}” wasn't found. Load it to name the talkgroups.` });
        break;
      }
      case "units": {
        const sys = c.systems.find((x) => x.shortName === item.system);
        const conv = c.conventional.find((x) => x.shortName === item.system);
        if ((sys || conv) && !s.unitNames[item.system])
          out.push({
            item,
            target: sys || !conv ? `units-${item.system}` : `convunits-${item.system}`,
            title: `Unit names for ${item.system}`,
            text: `“${item.file}” (unitTagsFile) wasn't found. Load it to name the radios.`,
          });
        break;
      }
      case "channels": {
        const v = c.conventional.find((x) => x.shortName === item.system);
        if (v && !v.channelFile && v.channels.length <= item.had)
          out.push({ item, target: `channels-${item.system}`, title: `Channels for ${item.system}`, text: `“${item.file}” wasn't found. Link it here, or bring it in with Import CSV.` });
        break;
      }
      case "siteLock": {
        const sys = c.systems.find((x) => x.shortName === item.system);
        const k = item.site ?? 0;
        const site = sys?.sites[k];
        if (sys && site && site.expect.site == null)
          out.push({
            item,
            target: `site-${item.system}-${k}`,
            title: `Site lock for ${sys.sites.length > 1 ? `${siteNameOf(sys, site)} of ${item.system}` : item.system}`,
            text: `Trunk Recorder followed only site ${item.siteId}. Fill in the site number — a first run shows the one the control channel announces.`,
          });
        break;
      }
      case "source": {
        const src = c.sources[item.index];
        if (!src || src.type !== "rtlsdr" || src.serial !== item.serial) break;
        const dev = s.devices.find((d) => d.serial === item.serial);
        if (dev && !dev.busy) break;
        out.push({
          item,
          target: `src-${item.index}`,
          title: sourceTitle(c, item.index),
          text: dev ? `Dongle ${item.serial} is ${dev.busy}.` : `Dongle ${item.serial} isn't plugged in. Plug it in, or choose another dongle.`,
        });
        break;
      }
      case "driver": {
        const src = c.sources[item.index];
        const d = s.radios?.[item.driver];
        if (!src || src.type !== item.driver || !s.radios || d?.available) break;
        out.push({ item, target: `src-${item.index}`, title: sourceTitle(c, item.index), text: `Needs ${SOURCE_KINDS[item.driver].driver?.name} installed on this computer, then a restart.` });
        break;
      }
      case "squelch":
        if (c.conventional.find((x) => x.shortName === item.system)?.channels.length)
          out.push({ item, target: `squelch-${item.system}`, title: `Squelch for ${item.system}`, text: "Here it's how far above the noise a channel must rise (default 8 dB), not Trunk Recorder's level. Check it once recording." });
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
        const sites = sys?.enabled ? sys.sites.filter((x) => x.enabled && x.controlChannelsHz.length) : [];
        if (!sys || !sites.length) break;
        if (sites.some((x) => x.controlChannelsHz.some((f) => receiverOf(c, sys.shortName, f) >= 0))) break;
        const ccs = sites.flatMap((x) => x.controlChannelsHz);
        out.push({
          item,
          target: `cc-${item.system}-${sys.sites.indexOf(sites[0])}`,
          title: `Control channel of ${item.system}`,
          text: `No radio hears ${ccs.map((f) => formatMhz(f, 4)).join(" or ")} MHz. Move a radio's center frequency there, or add a radio.`,
        });
        break;
      }
    }
  }
  return out;
}

/** One thing waiting on you ([`waiting`]): an import's to-do, a plugin change held until Apply, or a restart. */
export interface Waiting {
  title: string;
  text: string;
  /** Where it is: a setup field (`need-<target>`), or a plugin's own page (`page`, its id). Neither: the list itself. */
  target?: string;
  page?: string;
  /** What it is: the import's item, the form held, or changes that take effect on the next Start. */
  todo?: ImportTodo;
  held?: HeldForm;
  restart?: true;
}

/** Where form `form`'s Apply bar is: a setup field id (`need-<target>`), or its plugin's own page. */
export function heldPlace(s: AppState, form: HeldForm): { target?: string; page?: string } {
  if (form.kind === "settings") {
    const own = s.plugins?.plugins.find((p) => p.id === form.plugin)?.manifest?.page;
    return own ? { page: form.plugin } : { target: `plugin-${form.plugin}-held` };
  }
  const sys = s.config && systemAt(s.config, form.system);
  const at = `${typeof form.system === "number" ? "sysplug" : "convplug"}-${sys?.shortName ?? ""}-${form.plugin}-${form.copy}`;
  return { target: `${at}-${form.kind === "site" ? `site${form.site}` : form.kind}-held` };
}

/** Everything waiting on you, in one list: the import's to-dos, held plugin changes, then a restart. */
export function waiting(s: AppState): Waiting[] {
  const recording = isRecording(s.phase);
  return [
    ...openTodos(s).map((t) => ({ title: t.title, text: t.text, target: t.target, todo: t.item })),
    ...s.held.map((h) => ({ title: `${h.name}: not applied`, text: `Apply restarts ${h.name}${recording ? "" : " when recording starts"}`, held: h.form, ...heldPlace(s, h.form) })),
    ...(recording && s.restartNeeded ? [{ title: "Restart to apply", text: "Some changes take effect on the next Start", restart: true as const }] : []),
  ];
}
